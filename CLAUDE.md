# CLAUDE.md

See [AGENTS.md](AGENTS.md) for the full agent guide (architecture, tests, CLI contract, conventions).

Key commands (Windows, MSVC toolchain):

```
cargo +1.96.1-x86_64-pc-windows-msvc test --workspace --locked
cargo +1.96.1-x86_64-pc-windows-msvc clippy --workspace --all-targets --locked -- -D warnings
cargo +1.96.1-x86_64-pc-windows-msvc build --release --locked
target\release\archiver.exe --screenshot docs\screenshot.png --size 1100x720
```

Do not run `cargo clean` as a routine fix.
