# AGENTS.md

Guide for AI coding agents working in this repository. Human-facing docs: [README.md](README.md) (Traditional Chinese, with an English summary at the end). Version history: [docs/CHANGELOG.md](docs/CHANGELOG.md).

## Purpose

Archiver is a cross-platform (Windows, Linux, macOS) archive manager. One Cargo workspace (`core`, `gui`) produces three things:

- **`archiver-core`** (`core/`): library. List, extract, test, create, add/delete entries, ZIP comments, batch operations, single-entry reads for previews.
- **`archiver`** (`gui/src/main.rs` and `gui/src/app`, `gui/src/ui`): desktop GUI on AcmeUI-Native. It is also the entry point for shell integration and the headless `--screenshot` mode.
- **`rarc`** (`gui/src/bin/rarc.rs`): command-line tool for people and scripts. Its `--json` output and exit codes are a stable contract.

## Module map

| Path | Role |
|---|---|
| `core/src/lib.rs` | `Format` (extensions, levels, passwords, read/write capability), `Error`, `Entry`, `Progress`, public API re-exports |
| `core/src/archive_ops.rs` | list, extract (ZIP parallel, 7z, tar.*, RAR), test, delete |
| `core/src/create.rs` | create archives |
| `core/src/edit.rs` | `read_comment`, `set_comment` (ZIP), `add` (ZIP append; 7z/tar.* rewrite) |
| `core/src/batch.rs` | `create_each`, `extract_each` (per-item results, one failure does not stop the batch) |
| `core/src/preview.rs` | read a single entry for the preview pane |
| `gui/src/main.rs` | entry point: argument dispatch, `collect` merge, `screenshot()`, `App` implements `acme_platform::Application` |
| `gui/src/args.rs` | `parse_invocation`, `Action`, `USAGE` text |
| `gui/src/app/model.rs` | `Model` (all state), `Mode` (filesystem or archive), `Dialog`, `Popup`, `JobView` |
| `gui/src/app/msg.rs` | `Msg` (every input and worker result), `Cmd` (menu/toolbar/shortcut commands), `KeyPress`, `DlgMsg` |
| `gui/src/app/update.rs` | `update(m, msg, fx)` and the handlers behind it |
| `gui/src/app/effects.rs` | `Effects` trait and `RealEffects` (rfd dialogs, threads, event-loop waker) |
| `gui/src/app/menus.rs` | menu bar, toolbar and context menu contents as data |
| `gui/src/app/worker.rs` | background helpers |
| `gui/src/ui/` | immediate-mode painter: `mod.rs` (bands, hit regions), `dialogs.rs`, `widgets.rs`, `text.rs`, `icons.rs` |
| `gui/src/shell.rs` | context menus: Windows HKCU registry, Linux `.desktop`/Nautilus/Dolphin/Nemo, macOS Quick Actions; legacy cleanup |
| `gui/src/collect.rs` | Explorer multi-select merge (spool directory and leader lock) |
| `gui/src/favorites.rs` | favorites file in the per-user config directory |
| `gui/src/scan.rs` | Microsoft Defender scan (Windows only) |
| `gui/src/view.rs` | list item formatting, temp and clipboard staging directories |
| `gui/src/preview.rs` | preview classification (text, image, hex) |
| `gui/src/clipboard.rs` | system clipboard (file list and text) |
| `gui/src/os.rs` | open with default app, dark/light detection |
| `gui/src/tests.rs` | headless GUI state-machine tests (declared in `main.rs`) |
| `gui/tests/rarc_cli.rs` | rarc integration tests |
| `gui/build.rs`, `gui/assets/` | Windows icon resource; SVG icons |
| `installer/archiver.iss` | Inno Setup script |
| `scripts/` | `build-installer.ps1`, `installer-smoke.ps1`, `macos-bundle.sh`, `Info.plist` |
| `.github/workflows/` | `ci.yml` (clippy, build, tests, rarc and registry smoke tests, screenshot, installer), `release.yml` |

## Architecture

The GUI follows Model / Msg / update / Effects:

- **`app::Model`** holds all state: mode (filesystem or open archive), items, selection, dialogs, popups, running job, preview state, favorites.
- **`app::Msg`** is every input: commands (`Msg::Cmd(Cmd)`), pointer, keyboard and dialog messages, and results from background work (`Msg::JobDone`, preview and listing results, `Msg::Tick`).
- **`app::update(m, msg, fx)`** is the only place state changes. It does no OS work itself. All side effects go through `fx: &mut dyn Effects`.
- **`app::Effects`** covers pickers, message boxes, open-file, `spawn(JobFn)`, clipboard, shell registration, favorites and close. `JobFn` is `FnOnce(&Progress) -> Msg`, so a job returns the message that updates the model. `RealEffects` implements the trait; tests use a recorder.
- **`app::menus`** builds entries (label, enabled, checked, message). Menu state is tested through this data.
- **`ui/`** paints an `acme_core` `Scene` from `&Model` every frame and records hit regions that map back to `Msg`. It holds only layout and hit-test state.
- **`main.rs`** translates platform events into `Msg`, calls `update`, drains `RealEffects` results, and requests redraws.

Rules that follow from this:

- Never block inside `update`. Put slow work in `fx.spawn` (or the `update::spawn` helper) and return a `Msg` from the job.
- Keep OS calls behind `Effects` so the logic stays testable.
- Pure modules (`view.rs`, `preview.rs`, `clipboard.rs` data, `args.rs`, `shell.rs` generators) have no window dependency.

## Headless GUI tests

Write new GUI logic tests in `gui/src/tests.rs`. Recipe:

```rust
let dir = tempfile::tempdir().unwrap();
let mut m = Model::new(dir.path().to_path_buf());
let mut fx = Fx::default();              // recorder: logs effects, queues jobs, scripted pickers
fx.files = Some(vec![sample_zip(dir.path())]); // what pick_file / pick_files return
update(&mut m, Msg::Cmd(Cmd::Open), &mut fx);
fx.run_jobs(&mut m);                      // run queued background jobs synchronously
assert_eq!(m.items.len(), 3);
assert!(fx.log.iter().any(|l| l.starts_with("pick_file")));
```

- `Fx` (in `tests.rs`) implements `Effects`. Its fields script the pickers (`folder`, `files`), `confirm`, and clipboard/favorites/shell state. Check `fx.log` for effects.
- Shell registration tests write to a test hive (`Software\ArchiverTest\Classes`), not the real registry.
- Collect tests use `collect::gather_with` with a temp `Config`.
- Nothing in the test suite opens a window. Visual checks use `--screenshot` (CPU renderer).

## Build, test, lint

This Windows machine's default Rust toolchain is GNU and fails to link. Use the MSVC toolchain explicitly:

```
cargo +1.96.1-x86_64-pc-windows-msvc build --release --locked
cargo +1.96.1-x86_64-pc-windows-msvc test --workspace --locked
cargo +1.96.1-x86_64-pc-windows-msvc clippy --workspace --all-targets --locked -- -D warnings
```

On Linux and macOS, drop the `+toolchain` part. CI runs the same clippy command (with `-D warnings`), a release build, the tests, then the smoke tests below.

- Use `--locked`. `Cargo.lock` must stay in sync; do not hand-edit it.
- The first build fetches AcmeUI-Native from GitHub at the pinned rev `c2e0fd9765d0f25119a240ad1adb378c045df63f` (see `gui/Cargo.toml`).
- The release profile uses fat LTO and is slow. Iterate with `cargo test` (debug) and only build release when needed.
- Do not run `cargo clean` as a routine fix. It forces a full rebuild, including LTO and the git dependencies. Investigate the actual error first.
- Installer: `pwsh scripts/build-installer.ps1` (needs a release build and Inno Setup 6). Smoke test: `pwsh scripts/installer-smoke.ps1`.

## Headless screenshot

```
target\release\archiver.exe --screenshot docs\screenshot.png --size 1100x720 [archive-or-folder]
```

Renders the main window with the CPU renderer and exits (0 on success). No display or GPU needed. The optional path opens an archive or a folder first. CI runs this step on Windows and fails if the PNG is under 5000 bytes. Regenerate `docs/screenshot.png` only when the main window's look changes. Sample data for it: create an archive with `rarc c sample.zip <folder>` and pass `sample.zip`.

## CLI contract for automation (rarc)

- Always pass `--json` (global flag). Each command prints exactly one JSON document on stdout. Never parse the human-readable output.
- Exit codes:
  - `0` ok
  - `1` failed
  - `2` usage error (clap, printed on stderr, not JSON)
  - `3` password required or wrong (detected from error text containing "password")
  - `4` partial batch failure (`ce`, `xe`: some items failed, some succeeded)
  - `ce` and `xe` with every item failed: `3` if every failure is a password error, otherwise `1`.
- Success shapes:
  - `l`: `{"ok":true,"archive","count","entries":[{"name","size","compressed","modified","is_dir","crc32","encrypted"}]}`
  - `t`: `{"ok":true,"archive","tested"}`
  - `x`: `{"ok":true,"archive","out"}`
  - `c`: `{"ok":true,"output"}`
  - `a`: `{"ok":true,"archive","prefix","added"}`
  - `comment show`: `{"ok":true,"archive","comment"}` (`null` when none); `comment set`: `{"ok":true,"archive"}`
  - `ce`, `xe`: `{"ok","total","failed","results":[{"input","output","ok","error","kind"}]}`
- Failure shape: `{"ok":false,"error":"...","kind":"..."}`, where `kind` is one of `password`, `io`, `unsupported`, `cancelled`, `not_found`, `format`.
- Smoke test (same as CI):

```
rarc c smoke/t.zip smoke/in --json | grep -q '"ok":true'
rarc l smoke/t.zip --json | grep -q '"name":"in/a.txt"'
rarc t smoke/t.zip --json | grep -q '"tested":1'
```

## Shell integration

- Registry verb keys are `01open` … `08compress7z` (see `COMMANDS` in `gui/src/shell.rs`). CI checks `02extracthere`, `05extractdest`, `06test`, `07compresszip`, `08compress7z`, `MultiSelectModel` (and `CommandFlags` as `REG_DWORD`).
- Keep keys 01 to 08 stable (09 was used briefly during v0.7.0 development and removed as a duplicate of 03; don't reuse it). Add new verbs at the next number and append them to `COMMANDS`. Do not renumber or reuse a key, because upgrades and CI depend on them.
- `--collect` must stay the first argument (`args::parse_invocation`). The verb names passed to `collect::gather` are part of the spool-directory layout.
- `register()` deletes the roots before rewriting them, so removed verbs disappear. `cleanup_legacy()` removes v0.2.0 "Rust Archiver" entries. Keep it until v0.2.0 installs are no longer a concern.
- Windows writes only under `HKCU\Software\Classes`. Never write to HKLM from code paths that run without admin.

## Conventions

- User-facing strings are Traditional Chinese. Keep menu labels in `menus.rs`, Windows labels in `shell.rs` (`win_label`), and the README in sync.
- Core errors are `archiver_core::Error`. The GUI shows their text; rarc maps them to `kind` and exit codes. `is_password_error` relies on the word "password" in error text, so keep that wording in core errors.
- RAR is read-only. Do not add RAR creation (the UnRAR license forbids it).
- Keep single-file formats (gz, bz2, xz, zst) out of the add and delete paths. `core/src/edit.rs` rejects them for `add`, and the GUI gates delete with `Model::archive_writable`.
- Do not change the AcmeUI pin casually. If you bump it, update the README build section and re-check the `ACME_RENDER_BACKEND` semantics in `acme-platform`.
- Version numbers are set by the maintainer. Do not change `version` in `Cargo.toml` unless asked.
- Docs: `README.md` (zh-TW, with an English summary), `AGENTS.md`, `CLAUDE.md` (pointer to this file), `docs/CHANGELOG.md`, `bench/README.md`. Add a CHANGELOG entry for user-visible changes.
- Keep commits focused. Stage explicit files. Never stage `.claude/`, `.env`, or credentials. Commit or push only when asked.

## Do not

- Do not run `cargo clean` as a routine fix.
- Do not push, open pull requests, or publish releases unless asked.
- Do not hand-edit `Cargo.lock`.
- Do not reintroduce Slint or eframe references. Both were removed on this branch.
- Do not break `--screenshot`. CI relies on it.
- Do not renumber the registry verb keys.
- Do not parse rarc's human-readable output in scripts. Use `--json` and the exit codes.
