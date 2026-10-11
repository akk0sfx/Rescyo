"""Wrap every corpus image into virtual-disk containers with the reference tools.

For each /out/<scenario>/image.img this writes /out/<scenario>/containers/ with:

    fixed.vhd, dynamic.vhd            qemu-img, format vpc
    dynamic.vhdx                      qemu-img
    sparse.vmdk, stream.vmdk          qemu-img, monolithicSparse and streamOptimized
    split.vmdk (+ split-f*.vmdk)      qemu-img, twoGbMaxExtentFlat (descriptor + flat extent)
    plain.qcow2, compressed.qcow2     qemu-img, the second with zlib-compressed clusters
    overlay.qcow2                     a qcow2 on top of plain.qcow2 with one 64 KiB write
    evidence.E01, .E02, …             ewfacquire, EnCase 6, deflate, 1 MiB segments
    containers.json                   what each file must read as

Every container must read back as image.img byte for byte, except overlay.qcow2, whose first
64 KiB are 0xAB (written through qemu-io).
"""

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path


def run(*cmd):
    subprocess.run(cmd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def version(cmd):
    out = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True).stdout
    return out.strip().splitlines()[0] if out.strip() else "unknown"


def wrap(scenario_dir: Path):
    image = scenario_dir / "image.img"
    if not image.is_file():
        return
    out = scenario_dir / "containers"
    if out.exists():
        shutil.rmtree(out)
    out.mkdir()
    q = ["qemu-img", "convert", "-f", "raw"]
    run(*q, "-O", "vpc", "-o", "subformat=fixed,force_size=on", str(image), str(out / "fixed.vhd"))
    run(*q, "-O", "vpc", "-o", "subformat=dynamic,force_size=on", str(image), str(out / "dynamic.vhd"))
    run(*q, "-O", "vhdx", str(image), str(out / "dynamic.vhdx"))
    run(*q, "-O", "vmdk", "-o", "subformat=monolithicSparse", str(image), str(out / "sparse.vmdk"))
    run(*q, "-O", "vmdk", "-o", "subformat=streamOptimized", str(image), str(out / "stream.vmdk"))
    run(*q, "-O", "vmdk", "-o", "subformat=twoGbMaxExtentFlat", str(image), str(out / "split.vmdk"))
    run(*q, "-O", "qcow2", str(image), str(out / "plain.qcow2"))
    run(*q, "-O", "qcow2", "-c", str(image), str(out / "compressed.qcow2"))
    run("qemu-img", "create", "-f", "qcow2", "-b", "plain.qcow2", "-F", "qcow2", str(out / "overlay.qcow2"))
    run("qemu-io", "-f", "qcow2", "-c", "write -P 0xab 0 64k", str(out / "overlay.qcow2"))
    run(
        "ewfacquire", "-u", "-q",
        "-t", str(out / "evidence"),
        "-f", "encase6",
        "-c", "deflate:fast",
        "-S", "1MiB",
        "-b", "64",
        "-C", "case", "-D", "rescyo corpus", "-E", "1", "-e", "rescyo", "-N", "none",
        str(image),
    )
    manifest = {
        "image": "image.img",
        "size": image.stat().st_size,
        "containers": [
            {"file": "fixed.vhd", "format": "vhd"},
            {"file": "dynamic.vhd", "format": "vhd"},
            {"file": "dynamic.vhdx", "format": "vhdx"},
            {"file": "sparse.vmdk", "format": "vmdk"},
            {"file": "stream.vmdk", "format": "vmdk"},
            {"file": "split.vmdk", "format": "vmdk"},
            {"file": "plain.qcow2", "format": "qcow2"},
            {"file": "compressed.qcow2", "format": "qcow2"},
            {"file": "overlay.qcow2", "format": "qcow2", "overwritten": [[0, 65536, 171]]},
            {"file": "evidence.E01", "format": "e01"},
        ],
        "tools": {
            "qemu-img": version(["qemu-img", "--version"]),
            "ewfacquire": version(["ewfacquire", "-V"]),
        },
    }
    (out / "containers.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"{scenario_dir.name}: {len(manifest['containers'])} containers")


def main():
    root = Path("/out")
    only = set(sys.argv[1:])
    for d in sorted(p for p in root.iterdir() if p.is_dir()):
        if not only or d.name in only:
            wrap(d)
    # Files written as root inside the container: hand them back to the invoking user.
    uid, gid = os.environ.get("HOST_UID"), os.environ.get("HOST_GID")
    if uid and gid:
        for p in root.rglob("containers*"):
            for f in [p, *p.rglob("*")] if p.is_dir() else [p]:
                os.chown(f, int(uid), int(gid))


if __name__ == "__main__":
    main()
