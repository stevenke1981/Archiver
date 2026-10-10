//! Command-line arguments used by the GUI binary (and by the shell context menus).

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// No arguments: plain GUI.
    None,
    Open(PathBuf),
    ExtractHere(Vec<PathBuf>),
    ExtractToFolder(Vec<PathBuf>),
    Compress(Vec<PathBuf>),
    RegisterShell,
    UnregisterShell,
    Help,
}

pub const USAGE: &str = "Usage: archiver [OPTION] [FILES...]
  <archive>                       open archive in the GUI
  --open <archive>                open archive in the GUI
  --extract-here <archives...>    extract next to each archive
  --extract-to-folder <archives...>  extract into <dir>/<archive name>/
  --compress <files/folders...>   open the create-archive dialog prefilled
  --register-shell                install right-click menu integration (current user)
  --unregister-shell              remove right-click menu integration
  -h, --help                      show this help";

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
        assert_eq!(parse(["--compress", "d", "f.txt"]), Ok(Action::Compress(p(&["d", "f.txt"]))));
        assert_eq!(parse(["f1", "f2"]), Ok(Action::Compress(p(&["f1", "f2"]))));
        assert_eq!(parse(["--register-shell"]), Ok(Action::RegisterShell));
        assert_eq!(parse(["--unregister-shell"]), Ok(Action::UnregisterShell));
        assert_eq!(parse(["--help"]), Ok(Action::Help));
    }

    #[test]
    fn errors() {
        assert!(parse(["--extract-here"]).is_err());
        assert!(parse(["--compress"]).is_err());
        assert!(parse(["--open"]).is_err());
        assert!(parse(["--open", "a", "b"]).is_err());
        assert!(parse(["--bogus"]).is_err());
    }
}
