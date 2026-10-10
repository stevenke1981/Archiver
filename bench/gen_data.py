#!/usr/bin/env python3
"""Generate a ~500 MB mixed benchmark data set (deterministic)."""
import os, random, sys
root = sys.argv[1] if len(sys.argv) > 1 else "bench-data"
rnd = random.Random(42)
words = [("".join(rnd.choice("abcdefghijklmnopqrstuvwxyz_") for _ in range(rnd.randint(2, 12)))) for _ in range(5000)]
def text(n):
    out, size = [], 0
    while size < n:
        line = " ".join(rnd.choice(words) for _ in range(rnd.randint(3, 14))) + ";\n"
        out.append(line); size += len(line)
    return "".join(out)[:n].encode()
os.makedirs(root, exist_ok=True)
# 1) 6000 small source-like files in nested dirs (~45 MB)
chunk = text(8 << 20)
for i in range(6000):
    d = os.path.join(root, "src", f"mod{i % 60:02d}", f"sub{i % 7}")
    os.makedirs(d, exist_ok=True)
    n = rnd.randint(500, 15000); off = rnd.randint(0, len(chunk) - n)
    open(os.path.join(d, f"file{i}.rs"), "wb").write(chunk[off:off + n])
# 2) large compressible: log + csv (~230 MB)
big = text(32 << 20)
with open(os.path.join(root, "app.log"), "wb") as f:
    for k in range(4): f.write(big)            # 128 MB
with open(os.path.join(root, "data.csv"), "wb") as f:
    for k in range(100000 * 10):
        f.write(f"{k},{rnd.randint(0,99999)},{rnd.random():.6f},{rnd.choice(words)}\n".encode())
# 3) incompressible (~250 MB): like media files
os.makedirs(os.path.join(root, "media"), exist_ok=True)
for i in range(5):
    open(os.path.join(root, "media", f"video{i}.mp4"), "wb").write(os.urandom(50 << 20))
# 4) semi-compressible binary (~40 MB)
open(os.path.join(root, "db.bin"), "wb").write(bytes((i * 7 + (i >> 9)) & 0xFF if i % 3 else rnd.getrandbits(8) for i in range(40 << 20)))
