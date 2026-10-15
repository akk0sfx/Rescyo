"""The file tree of the Apple images, with deterministic content, and its manifest."""
import hashlib, json, os, random, sys

root, manifest = sys.argv[1], sys.argv[2]
files = {}
def put(rel, size, seed):
    rnd = random.Random(seed)
    data = bytes(rnd.getrandbits(8) for _ in range(size))
    path = os.path.join(root, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)
    files[rel] = {"size": size, "sha256": hashlib.sha256(data).hexdigest()}

put("README.TXT", 1000, 1)
put("Документы/Отчёт за 2023 год.bin", 50_000, 2)
put("写真/東京タワー.bin", 70_001, 3)
put("deep/l1/l2/leaf.dat", 777, 4)
put("big/large.bin", 3_000_000, 5)
put("trash/deleted.bin", 200_000, 6)
put("trash/kept.txt", 4096, 7)
with open(manifest, "w") as f:
    json.dump(files, f, ensure_ascii=False, indent=1)
