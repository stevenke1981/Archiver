//! rarc — small command-line front-end for archiver-core.
use std::path::PathBuf;

use archiver_core::{BatchDest, BatchResult, CreateOptions, Format, Progress};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rarc", version, about = "Archiver CLI: list / extract / create archives")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List archive contents
    L { archive: PathBuf, #[arg(short, long)] password: Option<String> },
    /// Extract archive (optionally only given entries)
    X {
        archive: PathBuf,
        #[arg(short, long, default_value = ".")] out: PathBuf,
        #[arg(short, long)] password: Option<String>,
        entries: Vec<String>,
    },
    /// Create archive; format is taken from the output extension
    C {
        output: PathBuf,
        #[arg(required = true)] inputs: Vec<PathBuf>,
        #[arg(short, long)] level: Option<u32>,
        #[arg(short, long)] password: Option<String>,
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

/// Parses an archive format name (`Format::extension`, case-insensitive, optional leading dot).
fn parse_format(s: &str) -> Result<Format, String> {
    let want = s.trim_start_matches('.').to_ascii_lowercase();
    let formats = Format::writable();
    formats.iter().copied().find(|f| f.extension() == want.as_str()).ok_or_else(|| {
        let accepted: Vec<&str> = formats.iter().map(|f| f.extension()).collect();
        format!("unknown format '{s}' (accepted: {})", accepted.join(", "))
    })
}

/// Prints one line per batch item; returns true if any item failed.
fn report(rs: &[BatchResult]) -> bool {
    let mut failed = false;
    for r in rs {
        match &r.result {
            Ok(()) => println!("OK   {} -> {}", r.input.display(), r.output.display()),
            Err(e) => {
                failed = true;
                println!("FAIL {}: {e}", r.input.display());
            }
        }
    }
    failed
}

fn main() {
    let cli = Cli::parse();
    let r = match cli.cmd {
        Cmd::L { archive, password } => archiver_core::list(&archive, password.as_deref()).map(|es| {
            for e in es {
                println!("{:>12} {:>12} {:16} {}{}", e.size, e.compressed.map(|c| c.to_string()).unwrap_or("-".into()),
                    e.modified.unwrap_or_default(), e.name, if e.is_dir { "/" } else { "" });
            }
        }),
        Cmd::X { archive, out, password, entries } => {
            let sel = (!entries.is_empty()).then_some(entries);
            archiver_core::extract(&archive, &out, password.as_deref(), sel.as_deref(), &Progress::none())
        }
        Cmd::C { output, inputs, level, password } => match Format::detect(&output) {
            Some(format) => archiver_core::create(&output, &inputs, &CreateOptions { format, level, password }, &Progress::none()),
            None => Err(archiver_core::Error::Unsupported(output.display().to_string())),
        },
        Cmd::Ce { inputs, format, out, level, password } => {
            let rs = archiver_core::create_each(&inputs, out.as_deref(), &CreateOptions { format, level, password }, &Progress::none());
            std::process::exit(i32::from(report(&rs)));
        }
        Cmd::Xe { archives, out, password } => {
            let dest = out.map_or(BatchDest::BesideInFolder, BatchDest::IntoFolders);
            let rs = archiver_core::extract_each(&archives, &dest, password.as_deref(), &Progress::none());
            std::process::exit(i32::from(report(&rs)));
        }
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
