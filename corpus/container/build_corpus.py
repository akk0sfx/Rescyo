#!/usr/bin/env python3
"""Builds Rescyo's test images from scenario manifests.

Runs ONLY inside the corpus container (ADR-0002). File systems are created and populated by the
reference tools (dosfstools, mtools, exfatprogs, exfat-fuse, ntfs-3g) on regular files; FUSE
mounts happen inside the container. Output per scenario:

    /out/<scenario>/image.img       the disk image
    /out/<scenario>/expected.json   files with SHA-256, states, layout and damage targets

Usage: build_corpus.py <scenarios-dir> <out-dir> [scenario ...]
"""

import hashlib
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

SECTOR = 512
MIB = 1 << 20


def payload(seed: str, size: int) -> bytes:
    """Deterministic pseudo-random content: SHA-256 in counter mode."""
    out = bytearray()
    counter = 0
    while len(out) < size:
        out += hashlib.sha256(f"{seed}:{counter}".encode()).digest()
        counter += 1
    return bytes(out[:size])


# A 16x16 baseline JPEG; real-format files let content verification be tested too.
SMALL_JPEG = bytes.fromhex(
    "ffd8ffe000104a46494600010200000100010000ffc00011080010001003002200011101021101ffdb00430006040506050406060506070706080a100a0a09090a140e0f0c1017141818171416161a1d251f1a1b231c1616202c20232627292a29191f2d302d283025282928ffdb0043010707070a080a130a0a13281a161a2828282828282828282828282828282828282828282828282828282828282828282828282828282828282828282828282828ffc4001f0000010501010101010100000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffc4001f0100030101010101010101010000000000000102030405060708090a0bffc400b51100020102040403040705040400010277000102031104052131061241510761711322328108144291a1b1c109233352f0156272d10a162434e125f11718191a262728292a35363738393a434445464748494a535455565758595a636465666768696a737475767778797a82838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9faffda000c03000001110211003f00aada478192ccbdb3879148212dad06f6ed81e6305ef9ea3a7e14b145e1f8373c7a5ea1244b8c1f2adf71ff0080a93deba68575eb8f9bfb274f4936e14ac33aa8f4242e334f9f4af1b49344f685215db831da5a395ce7a9f3589c9cf638e3f3fab78ac4c67cb29cade728fe89bfccc2588c1e6368a9ca8cbf994e0975b5e293bebd95f65748ffd9"
)


def jpeg_sized(seed: str, size: int) -> bytes:
    """A valid JPEG of exactly `size` bytes (or the smallest valid one): comment segments after
    SOI carry deterministic filler."""
    body = SMALL_JPEG[2:]
    need = size - len(SMALL_JPEG)
    if need <= 0:
        return SMALL_JPEG
    need = max(need, 4)
    segs = []
    filler = payload(seed, need)
    pos = 0
    while need > 0:
        n = min(need, 65537)
        if 0 < need - n < 4:
            n -= 4
        data = filler[pos:pos + n - 4]
        pos += n - 4
        segs.append(b"\xff\xfe" + (len(data) + 2).to_bytes(2, "big") + data)
        need -= n
    return b"\xff\xd8" + b"".join(segs) + body


def ooxml_sized(seed: str, size: int, kind: str) -> bytes:
    """A valid OOXML package (docx or xlsx) padded with a stored part to `size` bytes."""
    import io
    import zipfile

    def build(pad: int) -> bytes:
        buf = io.BytesIO()
        fixed = (2020, 9, 13, 12, 0, 0)
        with zipfile.ZipFile(buf, "w") as z:
            def put(name, data, method):
                info = zipfile.ZipInfo(name, fixed)
                info.compress_type = method
                z.writestr(info, data)
            put("[Content_Types].xml", '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>', zipfile.ZIP_DEFLATED)
            main = "word/document.xml" if kind == "docx" else "xl/workbook.xml"
            put(main, f"<doc seed=\"{seed}\">" + payload(seed, 256).hex() + "</doc>", zipfile.ZIP_DEFLATED)
            put("docProps/rescyo-padding.bin", payload(seed + ":pad", pad), zipfile.ZIP_STORED)
        return buf.getvalue()

    base = build(0)
    return build(max(size - len(base), 0))


def content_for(path: str, seed: str, size: int) -> bytes:
    lower = path.lower()
    if lower.endswith((".jpg", ".jpeg")):
        return jpeg_sized(seed, size)
    if lower.endswith((".docx", ".xlsx")):
        return ooxml_sized(seed, size, lower.rsplit(".", 1)[1])
    return payload(seed, size)


def run(*cmd, **kw):
    r = subprocess.run([str(c) for c in cmd], capture_output=True, text=True, **kw)
    if r.returncode != 0:
        raise SystemExit(f"command failed ({r.returncode}): {' '.join(map(str, cmd))}\n{r.stderr}")
    return r


def tool_version(cmd):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True)
        text = (r.stdout + r.stderr).strip().splitlines()
        return text[0] if text else "?"
    except OSError:
        return "?"


# --------------------------------------------------------------------------- file-system adapters


class MtoolsFs:
    """FAT12/16/32 populated with mtools, without mounting anything."""

    def __init__(self, part: Path):
        self.part = part

    def _p(self, path):
        return f"::/{path}"

    def mkdir(self, path):
        run("mmd", "-i", self.part, self._p(path))

    def write(self, path, data, tmp: Path, mtime: int):
        tmp.write_bytes(data)
        os.utime(tmp, (mtime, mtime))
        run("mcopy", "-m", "-i", self.part, tmp, self._p(path))

    def delete(self, path):
        run("mdel", "-i", self.part, self._p(path))

    def rmdir(self, path):
        run("mrd", "-i", self.part, self._p(path))

    def reset_alloc_hint(self):
        """Forget FAT32's FSInfo next-free hint so the next file fills holes first-fit."""
        with open(self.part, "r+b") as fh:
            bs = fh.read(SECTOR)
            if bs[82:87] != b"FAT32":
                return
            bps, fsinfo = struct.unpack_from("<H", bs, 11)[0], struct.unpack_from("<H", bs, 48)[0]
            fh.seek(fsinfo * bps + 492)
            fh.write(struct.pack("<I", 0xFFFFFFFF))

    def fragmented(self, path):
        out = run("mshowfat", "-i", self.part, self._p(path)).stdout
        return len(re.findall(r"<\d+(?:-\d+)?>", out)) > 1

    def close(self):
        pass


class FuseFs:
    """exFAT or NTFS populated through a FUSE mount of the partition file inside the container."""

    def __init__(self, part: Path, kind: str):
        self.mnt = Path(tempfile.mkdtemp(prefix="fs-"))
        self.loop = None
        if kind == "ntfs":
            run("ntfs-3g", "-o", "big_writes", part, self.mnt)
        else:
            # exfat-fuse running as root insists on a block device: use a loop device of the
            # container's VM (never a host device; the container is the isolation, ADR-0002).
            self.loop = run("losetup", "--find", "--show", part).stdout.strip()
            run("mount.exfat-fuse", self.loop, self.mnt)

    def mkdir(self, path):
        (self.mnt / path).mkdir()

    def write(self, path, data, tmp, mtime: int):
        with open(self.mnt / path, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        os.utime(self.mnt / path, (mtime, mtime))

    def reset_alloc_hint(self):
        pass

    def delete(self, path):
        (self.mnt / path).unlink()

    def rmdir(self, path):
        (self.mnt / path).rmdir()

    def fragmented(self, path):
        return None  # not observable through FUSE

    def close(self):
        os.sync()
        run("fusermount3", "-u", self.mnt)
        if self.loop:
            run("losetup", "--detach", self.loop)
        self.mnt.rmdir()


# --------------------------------------------------------------------------- the file tree


def tree_ops(cluster: int, part_size: int):
    """The operations that populate a volume, in order. Deletions come last so freed clusters
    are not reused. Returns a list of (op, path, size_or_None, flags)."""
    small = part_size < 8 * MIB
    big = 256 * 1024 if small else 2 * MIB
    many = 40 if small else 300
    c = cluster
    ops = []
    d = lambda p: ops.append(("mkdir", p, None, {}))
    f = lambda p, n, **fl: ops.append(("write", p, n, fl))

    f("README.TXT", 1000, in_root=True)
    f("notes.txt", 300, in_root=True)
    for p in ["Документы", "Документы/Бухгалтерия", "Документы/Бухгалтерия/Архив",
              "Документы/Бухгалтерия/Архив/2019", "照片", "写真", "사진", "Mixed Ünïcödé",
              "deep", "deep/l1", "deep/l1/l2", "deep/l1/l2/l3", "deep/l1/l2/l3/l4",
              "deep/l1/l2/l3/l4/l5", "big", "sizes", "many", "frag", "trash", "olddir"]:
        d(p)
    f("Документы/Отчёт за 2023 год.docx", 3 * c + 17)
    f("Документы/Бухгалтерия/Баланс.xlsx", 20000)
    f("Документы/Бухгалтерия/Архив/2019/январь.txt", 0)
    f("照片/北京 天安门.jpg", 5 * c)
    f("写真/東京タワー.jpg", c + 1)
    f("사진/서울 남산타워.jpg", max(c - 1, 1))
    f("Mixed Ünïcödé/a very long file name that goes on and on and on to exceed one hundred "
      "characters in total length.bin", 4096)
    f("deep/l1/l2/l3/l4/l5/leaf.dat", 777)
    f("big/large.bin", big)
    f("sizes/one.bin", 1)
    f("sizes/sector.bin", 512)
    f("sizes/cluster.bin", c)
    f("sizes/cluster_plus_one.bin", c + 1)
    for i in range(many):
        f(f"many/file_{i:03d}.txt", 50 + (i * 37) % 150)
    # Files deleted at the end are written before the fragmentation step, so they are
    # contiguous on every file system (exFAT and NTFS cannot report fragmentation here).
    f("trash/deleted_photo.jpg", 4 * c + 100)
    f("trash/удалённый документ.txt", 900)
    f("olddir/a.txt", 1500)
    f("olddir/b.bin", 2 * c)
    # Fragmentation: fill, punch holes, then write a file larger than any hole.
    for i in range(8):
        f(f"frag/filler_{i}.bin", 3 * c, temp=True)
    for i in range(0, 8, 2):
        ops.append(("delete", f"frag/filler_{i}.bin", None, {"temp": True}))
    ops.append(("reset_hint", "", None, {}))
    f("frag/fragmented.bin", 10 * c)
    ops.append(("delete", "trash/deleted_photo.jpg", None, {}))
    ops.append(("delete", "trash/удалённый документ.txt", None, {}))
    ops.append(("delete", "olddir/a.txt", None, {}))
    ops.append(("delete", "olddir/b.bin", None, {}))
    ops.append(("rmdir", "olddir", None, {}))
    return ops


def populate(fs, ops, scenario):
    files, dirs = {}, {}
    tmp = Path(tempfile.mkdtemp(prefix="payload-")) / "data"
    # Even seconds (FAT resolution), fixed so images are comparable across builds.
    mtime = 1_600_000_000
    for op, path, size, flags in ops:
        if op == "mkdir":
            fs.mkdir(path)
            dirs[path] = "live"
        elif op == "write":
            data = content_for(path, f"{scenario}:{path}", size)
            mtime += 2
            fs.write(path, data, tmp, mtime)
            if not flags.get("temp"):
                files[path] = {
                    "path": path,
                    "size": len(data),
                    "sha256": hashlib.sha256(data).hexdigest(),
                    "state": "live",
                    "in_root": bool(flags.get("in_root")),
                    "fragmented": None,
                    "mtime": mtime,
                }
        elif op == "delete":
            if path in files:
                files[path]["state"] = "deleted"
            fs.delete(path)
        elif op == "reset_hint":
            fs.reset_alloc_hint()
        elif op == "rmdir":
            fs.rmdir(path)
            dirs[path] = "deleted"
    for path, rec in files.items():
        if rec["state"] == "live" and rec["size"] > 0:
            rec["fragmented"] = fs.fragmented(path)
    fs.close()
    shutil.rmtree(tmp.parent)
    return sorted(files.values(), key=lambda r: r["path"]), [
        {"path": p, "state": s} for p, s in sorted(dirs.items())
    ]


# --------------------------------------------------------------------------- volume creation


def make_volume(sc, part: Path):
    fs = sc["fs"]
    size = sc["size_mib"] * MIB
    cluster = sc["cluster"]
    hidden = sc.get("part_start_mib", 0) * MIB // SECTOR if sc["table"] != "none" else 0
    if fs in ("fat12", "fat16", "fat32"):
        bits = fs[3:]
        run("mkfs.fat", "-F", bits, "-S", SECTOR, "-s", cluster // SECTOR, "-h", hidden,
            "-i", "52455343", "-n", "RESCYO", "-C", part, size // 1024)
        return MtoolsFs(part)
    with open(part, "wb") as fh:
        fh.truncate(size)
    if fs == "exfat":
        run("mkfs.exfat", "-c", cluster, "-L", "RESCYO", part)
    elif fs == "ntfs":
        run("mkntfs", "-F", "-Q", "-s", SECTOR, "-c", cluster, "-p", hidden, "-H", 255, "-S", 63,
            "-L", "RESCYO", part)
    else:
        raise SystemExit(f"unknown fs {fs}")
    return FuseFs(part, fs)


def assemble(sc, part: Path, image: Path):
    table = sc["table"]
    part_size = part.stat().st_size
    if table == "none":
        shutil.copyfile(part, image)
        return 0, part_size
    start = sc.get("part_start_mib", 1) * MIB
    total = start + part_size + MIB  # room for the backup GPT
    with open(image, "wb") as fh:
        fh.truncate(total)
    types = {"fat12": "1", "fat16": "e", "fat32": "c", "exfat": "7", "ntfs": "7"}
    if table == "mbr":
        script = (f"label: dos\nlabel-id: 0x52455343\n"
                  f"start={start // SECTOR}, size={part_size // SECTOR}, type={types[sc['fs']]}\n")
    else:
        script = ("label: gpt\nlabel-id: 52455343-0000-4000-8000-000000000001\n"
                  f"start={start // SECTOR}, size={part_size // SECTOR}, "
                  "type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, "
                  "uuid=52455343-0000-4000-8000-000000000002\n")
    run("sfdisk", "--no-reread", "--no-tell-kernel", image, input=script)
    with open(part, "rb") as src, open(image, "r+b") as dst:
        dst.seek(start)
        shutil.copyfileobj(src, dst, 4 * MIB)
    return start, part_size


# --------------------------------------------------------------------------- layout and damage


def layout_of(fs, image: Path, p: int, part_size: int):
    with open(image, "rb") as fh:
        fh.seek(p)
        bs = fh.read(SECTOR)
    rng = lambda a, n: [a, a + n]
    if fs.startswith("fat"):
        bps, spc, rsv, nfats, root_ents, tot16, _, fatsz16 = struct.unpack_from("<HBHBHHBH", bs, 11)
        tot32, fatsz32, _, _, root_clus, _, bk = struct.unpack_from("<IIHHIHH", bs, 32)
        fatsz = fatsz16 or fatsz32
        clus = bps * spc
        fat1 = p + rsv * bps
        root_dir = fat1 + nfats * fatsz * bps
        root_len = root_ents * 32
        data = root_dir + ((root_len + bps - 1) // bps) * bps
        lay = {"bytes_per_sector": bps, "cluster": clus, "fat_offset": fat1, "fat_size": fatsz * bps,
               "fats": nfats, "data_offset": data, "root_cluster": root_clus if fs == "fat32" else 0}
        dmg = {"boot": rng(p, bps), "fat1": rng(fat1, fatsz * bps)}
        if nfats > 1:
            dmg["fat2"] = rng(fat1 + fatsz * bps, fatsz * bps)
        if fs == "fat32":
            dmg["backup_boot"] = rng(p + bk * bps, bps)
            dmg["boot_and_backup"] = rng(p, (bk + 3) * bps)
            dmg["root"] = rng(data + (root_clus - 2) * clus, clus)
        else:
            dmg["root"] = rng(root_dir, root_len)
        return lay, dmg
    if fs == "exfat":
        fat_off, fat_len, heap, count, root = struct.unpack_from("<IIIII", bs, 80)
        bps = 1 << bs[108]
        clus = bps << bs[109]
        lay = {"bytes_per_sector": bps, "cluster": clus, "fat_offset": p + fat_off * bps,
               "fat_size": fat_len * bps, "data_offset": p + heap * bps, "cluster_count": count,
               "root_cluster": root}
        dmg = {"boot": rng(p, 12 * bps), "backup_boot": rng(p + 12 * bps, 12 * bps),
               "boot_and_backup": rng(p, 24 * bps), "fat1": rng(p + fat_off * bps, fat_len * bps),
               "root": rng(p + heap * bps + (root - 2) * clus, clus)}
        return lay, dmg
    if fs == "ntfs":
        bps, spc = struct.unpack_from("<HB", bs, 11)
        mft, mirr, cpr = struct.unpack_from("<qqb", bs, 48)
        clus = bps * spc
        rec = clus * cpr if cpr > 0 else 1 << -cpr
        lay = {"bytes_per_sector": bps, "cluster": clus, "mft_offset": p + mft * clus,
               "mftmirr_offset": p + mirr * clus, "record_size": rec}
        dmg = {"boot": rng(p, bps), "backup_boot": rng(p + part_size - bps, bps),
               "mft_head": rng(p + mft * clus, 4 * rec), "mftmirr": rng(p + mirr * clus, 4 * rec),
               "root_record": rng(p + mft * clus + 5 * rec, rec)}
        return lay, dmg
    raise SystemExit(f"unknown fs {fs}")


# --------------------------------------------------------------------------- main


def build(manifest: Path, out_root: Path):
    sc = tomllib.loads(manifest.read_text())
    name = sc["name"]
    out = out_root / name
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    work = Path(tempfile.mkdtemp(prefix=f"{name}-"))
    part = work / "part.img"
    fs = make_volume(sc, part)
    files, dirs = populate(fs, tree_ops(sc["cluster"], sc["size_mib"] * MIB), name)
    image = out / "image.img"
    start, part_size = assemble(sc, part, image)
    layout, damage = layout_of(sc["fs"], image, start, part_size)
    if sc["table"] != "none":
        damage["mbr"] = [0, SECTOR]
    expected = {
        "format": 1,
        "scenario": name,
        "fs": sc["fs"],
        "table": sc["table"],
        "image": "image.img",
        "image_size": image.stat().st_size,
        "partition": {"offset": start, "size": part_size},
        "layout": layout,
        "damage": damage,
        "corruptions": sc.get("corruptions", ["none"]),
        "files": files,
        "dirs": dirs,
        "tools": {
            "mkfs.fat": tool_version(["mkfs.fat", "--version"]),
            "mtools": tool_version(["mtools", "--version"]),
            "mkfs.exfat": tool_version(["mkfs.exfat", "-V"]),
            "ntfs-3g": tool_version(["ntfs-3g", "--version"]),
        },
    }
    (out / "expected.json").write_text(json.dumps(expected, ensure_ascii=False, indent=2) + "\n")
    shutil.rmtree(work)
    print(f"{name}: {len(files)} files, {image.stat().st_size // MIB} MiB")


def main():
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    scen_dir, out_root = Path(sys.argv[1]), Path(sys.argv[2])
    only = set(sys.argv[3:])
    for manifest in sorted(scen_dir.glob("*.toml")):
        if only and manifest.stem not in only:
            continue
        build(manifest, out_root)


if __name__ == "__main__":
    main()
