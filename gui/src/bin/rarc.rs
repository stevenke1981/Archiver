//! rarc — small command-line front-end for archiver-core.
//!
//! Exit codes: 0 ok, 1 failed, 2 usage error, 3 password required / wrong password,
//! 4 partial batch failure (some items of ce / xe failed, some succeeded).
//! With `--json` every command prints exactly one JSON document on stdout.
use std::path::PathBuf;

use archiver_core::{BatchDest, BatchResult, CreateOptions, Error, Format, Progress};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

const EXIT_OK: i32 = 0;
const EXIT_FAILED: i32 = 1;
const EXIT_PASSWORD: i32 = 3;
const EXIT_PARTIAL: i32 = 4;

const AFTER_HELP: &str = "Exit codes / 結束代碼:
  0 ok / 成功    1 failed / 失敗    2 usage error / 用法錯誤
  3 password required or wrong / 需要密碼或密碼錯誤
  4 partial batch failure / 批次部分失敗 (ce, xe)

--json prints one JSON document on stdout (machine-readable, 供程式讀取).";

#[derive(Parser)]
#[command(name = "rarc", version, about = "Archiver CLI: list / test / extract / create / edit archives (壓縮檔命令列工具)", after_help = AFTER_HELP)]
struct Cli {
    /// Machine-readable output: a single JSON document on stdout / 輸出 JSON
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List archive contents / 列出內容
    L { archive: PathBuf, #[arg(short, long)] password: Option<String> },
    /// Test archive integrity / 測試壓縮檔
    T { archive: PathBuf, #[arg(short, long)] password: Option<String> },
    /// Extract archive (optionally only given entries) / 解壓縮
    X {
        archive: PathBuf,
        #[arg(short, long, default_value = ".")] out: PathBuf,
        #[arg(short, long)] password: Option<String>,
        entries: Vec<String>,
    },
    /// Create archive; format is taken from the output extension / 建立壓縮檔
    C {
        output: PathBuf,
        #[arg(required = true)] inputs: Vec<PathBuf>,
        #[arg(short, long)] level: Option<u32>,
        #[arg(short, long)] password: Option<String>,
    },
    /// Add files/folders to an existing archive (ZIP in place, tar.*/7z rewritten; same names are replaced) / 加入檔案
    A {
        archive: PathBuf,
        #[arg(required = true)] inputs: Vec<PathBuf>,
        /// Folder inside the archive that receives the files (default: archive root)
        #[arg(long, default_value = "")] prefix: String,
        #[arg(short, long)] level: Option<u32>,
        /// Password for new entries (and for reading an encrypted tar/7z while rewriting it)
        #[arg(short, long)] password: Option<String>,
    },
    /// Show or set the archive comment (ZIP) / 壓縮檔註解
    Comment {
        #[command(subcommand)]
        cmd: CommentCmd,
    },
    /// Compress each input into its own archive (batch). Archive = <input name>.<format extension>
    Ce {
        #[arg(required = true)] inputs: Vec<PathBuf>,
        /// Format: zip, 7z, tar, tar.gz, tar.bz2, tar.xz, tar.zst, gz, bz2, xz, zst (case-insensitive)
        #[arg(short, long, value_parser = parse_format)] format: Format,
        /// Output folder for the archives (default: beside each input)
        #[arg(short, long)] out: Option<PathBuf>,
        #[arg(short, long)] level: Option<u32>,
        #[arg(short, long)] password: Option<String>,
    },
    /// Extract each archive into <stem>/ (batch); a failing archive does not stop the others
    Xe {
        #[arg(required = true)] archives: Vec<PathBuf>,
        /// Folder that receives the <stem>/ folders (default: beside each archive)
        #[arg(short, long)] out: Option<PathBuf>,
        #[arg(short, long)] password: Option<String>,
    },
}

#[derive(Subcommand)]
enum CommentCmd {
    /// Print the comment (empty output when there is none)
    Show { archive: PathBuf },
    /// Replace the comment; an empty text removes it
    Set {
        archive: PathBuf,
        #[arg(short, long)] message: String,
    },
}

/// Parses an archive format name (`Format::extension`, case-insensitive, optional leading dot).
fn parse_format(s: &str) -> Result<Format, String> {
    let want = s.trim_start_matches('.').to_ascii_lowercase();
    let formats = Format::writable();
    formats.iter().copied().find(|f| f.extension() == want.as_str()).ok_or_else(|| {
        let accepted: Vec<&str> = formats.iter().map(|f| f.extension()).collect();
        format!("unknown format '{s}' (accepted: {})", accepted.join(", "))
    })
}

/// The archive needs a password, or the one given is wrong (any format's wording).
fn is_password_error(e: &Error) -> bool {
    e.to_string().to_lowercase().contains("password")
}

fn kind(e: &Error) -> &'static str {
    if is_password_error(e) {
        return "password";
    }
    match e {
        Error::Io(_) => "io",
        Error::Unsupported(_) => "unsupported",
        Error::Cancelled => "cancelled",
        Error::NotFound(_) => "not_found",
        _ => "format",
    }
}

fn exit_code(e: &Error) -> i32 {
    if is_password_error(e) { EXIT_PASSWORD } else { EXIT_FAILED }
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string(v).unwrap_or_else(|_| "{}".into()));
}

fn path_str(p: &std::path::Path) -> String {
    p.display().to_string()
}

fn finish_err(json_out: bool, e: &Error) -> ! {
    if json_out {
        print_json(&json!({"ok": false, "error": e.to_string(), "kind": kind(e)}));
    } else {
        eprintln!("error: {e}");
    }
    std::process::exit(exit_code(e));
}

/// Report batch results and exit with 0 / 1 / 3 / 4.
fn finish_batch(json_out: bool, rs: &[BatchResult]) -> ! {
    let failed: Vec<&BatchResult> = rs.iter().filter(|r| r.result.is_err()).collect();
    if json_out {
        let items: Vec<Value> = rs
            .iter()
            .map(|r| match &r.result {
                Ok(()) => json!({"input": path_str(&r.input), "output": path_str(&r.output), "ok": true}),
                Err(e) => json!({"input": path_str(&r.input), "output": path_str(&r.output), "ok": false, "error": e.to_string(), "kind": kind(e)}),
            })
            .collect();
        print_json(&json!({"ok": failed.is_empty(), "total": rs.len(), "failed": failed.len(), "results": items}));
    } else {
        for r in rs {
            match &r.result {
                Ok(()) => println!("OK   {} -> {}", r.input.display(), r.output.display()),
                Err(e) => println!("FAIL {}: {e}", r.input.display()),
            }
        }
    }
    let code = if failed.is_empty() {
        EXIT_OK
    } else if failed.len() < rs.len() {
        EXIT_PARTIAL
    } else if failed.iter().all(|r| r.result.as_ref().err().is_some_and(is_password_error)) {
        EXIT_PASSWORD
    } else {
        EXIT_FAILED
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<(), Error> {
    let j = cli.json;
    let p = Progress::none();
    match cli.cmd {
        Cmd::L { archive, password } => {
            let es = archiver_core::list(&archive, password.as_deref())?;
            if j {
                let entries: Vec<Value> = es
                    .iter()
                    .map(|e| json!({"name": e.name, "size": e.size, "compressed": e.compressed, "modified": e.modified,
                        "is_dir": e.is_dir, "crc32": e.crc32.map(|c| format!("{c:08x}")), "encrypted": e.encrypted}))
                    .collect();
                print_json(&json!({"ok": true, "archive": path_str(&archive), "count": entries.len(), "entries": entries}));
            } else {
                for e in es {
                    println!("{:>12} {:>12} {:16} {}{}", e.size, e.compressed.map(|c| c.to_string()).unwrap_or("-".into()),
                        e.modified.unwrap_or_default(), e.name, if e.is_dir { "/" } else { "" });
                }
            }
        }
        Cmd::T { archive, password } => {
            let n = archiver_core::test(&archive, password.as_deref(), &p)?;
            if j {
                print_json(&json!({"ok": true, "archive": path_str(&archive), "tested": n}));
            } else {
                println!("OK   {} ({n} files tested)", archive.display());
            }
        }
        Cmd::X { archive, out, password, entries } => {
            let sel = (!entries.is_empty()).then_some(entries);
            archiver_core::extract(&archive, &out, password.as_deref(), sel.as_deref(), &p)?;
            if j {
                print_json(&json!({"ok": true, "archive": path_str(&archive), "out": path_str(&out)}));
            }
        }
        Cmd::C { output, inputs, level, password } => {
            let format = Format::detect(&output).ok_or_else(|| Error::Unsupported(output.display().to_string()))?;
            archiver_core::create(&output, &inputs, &CreateOptions { format, level, password }, &p)?;
            if j {
                print_json(&json!({"ok": true, "output": path_str(&output)}));
            }
        }
        Cmd::A { archive, inputs, prefix, level, password } => {
            let format = Format::detect(&archive).ok_or_else(|| Error::Unsupported(archive.display().to_string()))?;
            archiver_core::add(&archive, &inputs, &prefix, &CreateOptions { format, level, password }, &p)?;
            if j {
                let added: Vec<String> = inputs.iter().map(|i| path_str(i)).collect();
                print_json(&json!({"ok": true, "archive": path_str(&archive), "prefix": prefix, "added": added}));
            }
        }
        Cmd::Comment { cmd: CommentCmd::Show { archive } } => {
            let c = archiver_core::read_comment(&archive)?;
            if j {
                print_json(&json!({"ok": true, "archive": path_str(&archive), "comment": c}));
            } else if let Some(c) = c {
                println!("{c}");
            }
        }
        Cmd::Comment { cmd: CommentCmd::Set { archive, message } } => {
            archiver_core::set_comment(&archive, &message, &p)?;
            if j {
                print_json(&json!({"ok": true, "archive": path_str(&archive)}));
            }
        }
        Cmd::Ce { inputs, format, out, level, password } => {
            let rs = archiver_core::create_each(&inputs, out.as_deref(), &CreateOptions { format, level, password }, &p);
            finish_batch(j, &rs);
        }
        Cmd::Xe { archives, out, password } => {
            let dest = out.map_or(BatchDest::BesideInFolder, BatchDest::IntoFolders);
            let rs = archiver_core::extract_each(&archives, &dest, password.as_deref(), &p);
            finish_batch(j, &rs);
        }
    }
    Ok(())
}

fn main() {
    let cli = Cli::parse(); // usage errors exit with code 2
    let json_out = cli.json;
    if let Err(e) = run(cli) {
        finish_err(json_out, &e);
    }
}
