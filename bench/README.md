# Benchmark

```bash
cargo build --release
bench/run.sh                       # uses target/release/rarc, data in /tmp/bench-data (generated, ~490 MB)
FORMATS="zip tar.zst" bench/run.sh # subset
ARCHIVER_THREADS=1 bench/run.sh    # force single-threaded codecs
```

Data set (`gen_data.py`, deterministic): 6000 small source-like files, 128 MB log, ~40 MB CSV,
40 MB semi-compressible binary, 250 MB random (incompressible, like media). Requires GNU `time`.

Results on the dev box (Linux x86_64, 8 vCPU), v0.3.0 → v0.4.0:

| format | size v0.3 → v0.4 | create s | list s | extract s | peak RAM MB |
|---|---|---|---|---|---|
| zip | 386 MB → 386 MB (78.8%) | 10.49 → 2.43 | 0.02 → 0.01 | 2.35 → 1.40 | 9 → 25 |
| 7z | 338 MB (69.1%) → 318 MB (65.0%) | 292.0 → 45.9 | 0.02 → 0.01 | 11.08 → 1.28 | 196 → 1364 |
| tar.gz | 380 MB → 380 MB (77.6%) | 9.36 → 1.55 | 0.91 → 0.84 | 1.52 → 1.38 | 6 → 99 |
| tar.xz | 318 MB (64.9%) → 319 MB (65.1%) | 284.9 → 40.8 | 3.06 → 0.65 | 3.77 → 0.96 | 99 → 1198 |
| tar.zst | 350 MB → 350 MB (71.4%) | 2.00 → 0.55 | 0.31 → 0.28 | 0.78 → 0.67 | 10 → 171 |

Binary size (stripped, Linux x86_64): `archiver` 13.57 MB → 10.39 MB, `rarc` 4.00 MB → 3.13 MB.
