//! Command-line arguments used by the GUI binary (and by the shell context menus).

use std::path::PathBuf;

use archiver_core::Format;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// No arguments: plain GUI.
    None,
    Open(PathBuf),
    ExtractHere(Vec<PathBuf>),
    ExtractToFolder(Vec<PathBuf>),
    ExtractTo(Vec<PathBuf>),
    Compress(Vec<PathBuf>),
    /// Test archive integrity (context-menu "Test archive").
    Test(Vec<PathBuf>),
    /// One-click "compress to <name>.<ext>" next to the inputs, no dialog.
    CompressTo { format: Format, files: Vec<PathBuf> },
    /// Render the main window headlessly to a PNG (agents / CI), then exit.
    Screenshot { out: PathBuf, size: (u32, u32), open: Option<PathBuf> },
    RegisterShell,
    UnregisterShell,
    Help,
}

pub const USAGE: &str = "Usage: archiver [--collect] [OPTION] [FILES...]
  <archive>                       open archive in the GUI
  --open <archive>                open archive in the GUI
  --extract-here <archives...>    extract next to each archive
  --extract-to-folder <archives...>  extract into <dir>/<archive name>/
  --extract-to <archives...>      choose a destination folder for extraction
  --compress <files/folders...>   open the create-archive dialog prefilled
  --compress-to <fmt> <files...>  compress next to the inputs without a dialog
                                  (fmt: zip, 7z, tar, tar.gz, tar.bz2, tar.xz, tar.zst, gz, bz2, xz, zst)
  --test <archives...>            test archive integrity
  --screenshot <out.png> [--size WxH] [archive-or-folder]
                                  render the main window to a PNG and exit
  --collect                       (before an option) Explorer multi-select: merge the paths
                                  passed by every process of one selection into a single run
  --register-shell                install right-click menu integration (current user)
  --unregister-shell              remove right-click menu integration
  -h, --help                      show this help";

/// Default screenshot size.
pub const SCREENSHOT_SIZE: (u32, u32) = (1100, 720);

/// Format named on the command line (`zip`, `7z`, `tar.gz`, `tgz`, ...), writable formats only.
pub fn parse_format(s: &str) -> Result<Format, String> {
    let l = s.trim_start_matches('.').to_lowercase();
    let f = match l.as_str() {
        "tgz" => Some(Format::TarGz),
        "tbz2" | "tbz" => Some(Format::TarBz2),
        "txz" => Some(Format::TarXz),
        "tzst" => Some(Format::TarZst),
        other => Format::ALL.into_iter().find(|f| f.extension() == other),
    };
    match f {
        Some(f) if f.can_create() => Ok(f),
        Some(f) => Err(format!("cannot create {} archives", f.label())),
        None => Err(format!("unknown archive format '{s}'")),
    }
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once(['x', 'X']).ok_or_else(|| format!("invalid --size '{s}' (expected WxH)"))?;
    match (w.parse::<u32>(), h.parse::<u32>()) {
        (Ok(w), Ok(h)) if (100..=8000).contains(&w) && (100..=8000).contains(&h) => Ok((w, h)),
        _ => Err(format!("invalid --size '{s}' (expected WxH, 100..8000)")),
    }
}

/// Parsed command line plus the `--collect` flag (aggregate Explorer multi-select,
/// see `collect::gather`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub action: Action,
    pub collect: bool,
}

pub fn parse_invocation<I, S>(args: I) -> Result<Invocation, String>
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString>,
{
    let mut args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
    let collect = args.first().is_some_and(|a| a == "--collect");
    if collect {
        args.remove(0);
        if args.is_empty() {
            return Err("--collect must be followed by an option".into());
        }
    }
    parse(args).map(|action| Invocation { action, collect })
}

pub fn parse<I, S>(args: I) -> Result<Action, String>
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString>,
{
    let args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
    let Some(first) = args.first() else { return Ok(Action::None) };
    let rest = || -> Vec<PathBuf> { args[1..].iter().map(PathBuf::from).collect() };
    let need = |v: Vec<PathBuf>, opt: &str| if v.is_empty() { Err(format!("{opt} requires at least one file")) } else { Ok(v) };
    match first.to_str() {
        Some("--open") => match args.len() {
            2 => Ok(Action::Open(PathBuf::from(&args[1]))),
            _ => Err("--open requires exactly one archive".into()),
        },
        Some("--extract-here") => need(rest(), "--extract-here").map(Action::ExtractHere),
        Some("--extract-to-folder") => need(rest(), "--extract-to-folder").map(Action::ExtractToFolder),
        Some("--extract-to") => need(rest(), "--extract-to").map(Action::ExtractTo),
        Some("--test") => need(rest(), "--test").map(Action::Test),
        Some("--compress-to") => {
            let fmt = args.get(1).and_then(|s| s.to_str()).ok_or("--compress-to requires a format (zip, 7z, tar.gz, ...)")?;
            let format = parse_format(fmt)?;
            let files = need(args[2..].iter().map(PathBuf::from).collect(), "--compress-to")?;
            Ok(Action::CompressTo { format, files })
        }
        Some("--screenshot") => {
            let out = args.get(1).map(PathBuf::from).ok_or("--screenshot requires an output .png path")?;
            let (mut size, mut open, mut i) = (SCREENSHOT_SIZE, None, 2);
            while i < args.len() {
                if args[i] == "--size" {
                    size = parse_size(args.get(i + 1).and_then(|s| s.to_str()).ok_or("--size requires WxH")?)?;
                    i += 2;
                } else if open.is_none() {
                    open = Some(PathBuf::from(&args[i]));
                    i += 1;
                } else {
                    return Err("--screenshot takes at most one path".into());
                }
            }
            Ok(Action::Screenshot { out, size, open })
        }
        Some("--compress") => need(rest(), "--compress").map(Action::Compress),
        Some("--register-shell") => Ok(Action::RegisterShell),
        Some("--unregister-shell") => Ok(Action::UnregisterShell),
        Some("-h" | "--help") => Ok(Action::Help),
        Some(s) if s.starts_with("--") => Err(format!("unknown option {s}\n\n{USAGE}")),
        _ if args.len() == 1 => Ok(Action::Open(PathBuf::from(first))),
        // several plain paths (e.g. dropped on the executable): compress them
        _ => Ok(Action::Compress(args.iter().map(PathBuf::from).collect())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(v: &[&str]) -> Vec<PathBuf> {
        v.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn parses() {
        assert_eq!(parse(Vec::<String>::new()), Ok(Action::None));
        assert_eq!(parse(["a.zip"]), Ok(Action::Open("a.zip".into())));
        assert_eq!(parse(["--open", "a.7z"]), Ok(Action::Open("a.7z".into())));
        assert_eq!(parse(["--extract-here", "a.zip", "b c.rar"]), Ok(Action::ExtractHere(p(&["a.zip", "b c.rar"]))));
        assert_eq!(parse(["--extract-to-folder", "x.tar.gz"]), Ok(Action::ExtractToFolder(p(&["x.tar.gz"]))));
        assert_eq!(parse(["--extract-to", "中文 資料.zip", "b c.rar"]), Ok(Action::ExtractTo(p(&["中文 資料.zip", "b c.rar"]))));
        assert_eq!(parse(["--compress", "d", "f.txt"]), Ok(Action::Compress(p(&["d", "f.txt"]))));
        assert_eq!(parse(["f1", "f2"]), Ok(Action::Compress(p(&["f1", "f2"]))));
        assert_eq!(parse(["--register-shell"]), Ok(Action::RegisterShell));
        assert_eq!(parse(["--unregister-shell"]), Ok(Action::UnregisterShell));
        assert_eq!(parse(["--help"]), Ok(Action::Help));
        assert_eq!(parse(["--test", "a.zip", "b.7z"]), Ok(Action::Test(p(&["a.zip", "b.7z"]))));
        assert_eq!(
            parse(["--compress-to", "zip", "d", "f 1.txt"]),
            Ok(Action::CompressTo { format: Format::Zip, files: p(&["d", "f 1.txt"]) })
        );
        assert_eq!(parse(["--compress-to", "TAR.GZ", "d"]), Ok(Action::CompressTo { format: Format::TarGz, files: p(&["d"]) }));
        assert_eq!(parse(["--compress-to", "7z", "d"]), Ok(Action::CompressTo { format: Format::SevenZ, files: p(&["d"]) }));
    }

    #[test]
    fn screenshot() {
        assert_eq!(
            parse(["--screenshot", "o.png"]),
            Ok(Action::Screenshot { out: "o.png".into(), size: SCREENSHOT_SIZE, open: None })
        );
        assert_eq!(
            parse(["--screenshot", "o.png", "--size", "800x600", "a.zip"]),
            Ok(Action::Screenshot { out: "o.png".into(), size: (800, 600), open: Some("a.zip".into()) })
        );
        assert_eq!(
            parse(["--screenshot", "o.png", "dir", "--size", "640X480"]),
            Ok(Action::Screenshot { out: "o.png".into(), size: (640, 480), open: Some("dir".into()) })
        );
        assert!(parse(["--screenshot"]).is_err());
        assert!(parse(["--screenshot", "o.png", "--size"]).is_err());
        assert!(parse(["--screenshot", "o.png", "--size", "bad"]).is_err());
        assert!(parse(["--screenshot", "o.png", "--size", "10x10"]).is_err());
        assert!(parse(["--screenshot", "o.png", "a", "b"]).is_err());
    }

    #[test]
    fn collect_flag() {
        let i = parse_invocation(["--collect", "--test", "a.zip"]).unwrap();
        assert!(i.collect);
        assert_eq!(i.action, Action::Test(p(&["a.zip"])));
        let i = parse_invocation(["--compress-to", "zip", "a"]).unwrap();
        assert!(!i.collect);
        assert!(parse_invocation(["--collect"]).is_err());
        assert!(parse_invocation(["--collect", "--bogus"]).is_err());
        assert_eq!(parse_invocation(Vec::<String>::new()).unwrap(), Invocation { action: Action::None, collect: false });
    }

    #[test]
    fn errors() {
        assert!(parse(["--extract-here"]).is_err());
        assert!(parse(["--extract-to"]).is_err());
        assert!(parse(["--extract-to-folder"]).is_err());
        assert!(parse(["--compress"]).is_err());
        assert!(parse(["--open"]).is_err());
        assert!(parse(["--open", "a", "b"]).is_err());
        assert!(parse(["--bogus"]).is_err());
        assert!(parse(["--test"]).is_err());
        assert!(parse(["--compress-to"]).is_err());
        assert!(parse(["--compress-to", "zip"]).is_err());
        assert!(parse(["--compress-to", "rar", "a"]).is_err());
        assert!(parse(["--compress-to", "nope", "a"]).is_err());
    }
}
