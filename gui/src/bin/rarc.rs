//! rarc — small command-line front-end for archiver-core.
use std::path::PathBuf;

use archiver_core::{CreateOptions, Format, Progress};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rarc", version, about = "Rust Archiver CLI: list / extract / create archives")]
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
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
