//! Virus scan of an archive via Microsoft Defender (MpCmdRun.exe) on Windows.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum ScanOutcome {
    Clean,
    /// Defender reported a threat; text is the scanner output summary.
    Threat(String),
}

/// Whether a scanner is available on this system (false on non-Windows).
pub fn available() -> bool {
    #[cfg(windows)]
    {
        win::find_mpcmdrun().is_some()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Blocking scan of one file.
pub fn scan_file(path: &Path) -> Result<ScanOutcome, String> {
    #[cfg(windows)]
    {
        let exe = win::find_mpcmdrun().ok_or_else(|| "Microsoft Defender (MpCmdRun.exe) was not found".to_string())?;
        win::scan(&exe, path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("virus scan is not available on this platform".into())
    }
}

/// Map an MpCmdRun exit code: 0 = clean, 2 = threat found, anything else = scanner error.
#[cfg(any(windows, test))]
pub fn interpret(code: Option<i32>, output: &str) -> Result<ScanOutcome, String> {
    let summary = || {
        let s: Vec<&str> = output.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        let tail = s.len().saturating_sub(6);
        s[tail..].join("\n")
    };
    match code {
        Some(0) => Ok(ScanOutcome::Clean),
        Some(2) => Ok(ScanOutcome::Threat(summary())),
        Some(c) => Err(format!("scanner failed (exit code {c})\n{}", summary())),
        None => Err("scanner was terminated".into()),
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// Defender's command-line tool: the platform folder (newest version) first, else Program Files.
    pub fn find_mpcmdrun() -> Option<PathBuf> {
        let platform = std::env::var_os("ProgramData")
            .map(|d| PathBuf::from(d).join(r"Microsoft\Windows Defender\Platform"))
            .and_then(|d| latest_platform(&d));
        let program_files = std::env::var_os("ProgramFiles").map(|d| PathBuf::from(d).join(r"Windows Defender\MpCmdRun.exe"));
        platform.into_iter().chain(program_files).find(|p| p.is_file())
    }

    /// `<dir>/<newest version>/MpCmdRun.exe` (versions like 4.18.24090.11-0 compared numerically).
    fn latest_platform(dir: &Path) -> Option<PathBuf> {
        let mut best: Option<(Vec<u64>, PathBuf)> = None;
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let exe = e.path().join("MpCmdRun.exe");
            if !exe.is_file() {
                continue;
            }
            let v = version_key(&e.file_name().to_string_lossy());
            if best.as_ref().is_none_or(|b| v > b.0) {
                best = Some((v, exe));
            }
        }
        best.map(|b| b.1)
    }

    pub fn version_key(name: &str) -> Vec<u64> {
        name.split(['.', '-', '_']).filter_map(|p| p.parse().ok()).collect()
    }

    pub fn scan(exe: &Path, file: &Path) -> Result<ScanOutcome, String> {
        let out = Command::new(exe)
            .args(["-Scan", "-ScanType", "3", "-File"])
            .arg(file)
            .arg("-DisableRemediation")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
        interpret(out.status.code(), &String::from_utf8_lossy(&out.stdout))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn picks_newest_platform_version() {
            let t = tempfile::tempdir().unwrap();
            for v in ["4.18.9.1-0", "4.18.24090.11-0", "4.18.10.3-0"] {
                std::fs::create_dir_all(t.path().join(v)).unwrap();
                std::fs::write(t.path().join(v).join("MpCmdRun.exe"), b"").unwrap();
            }
            std::fs::create_dir_all(t.path().join("9.9.9-0")).unwrap(); // no exe: ignored
            let p = latest_platform(t.path()).unwrap();
            assert!(p.to_string_lossy().contains("4.18.24090.11-0"), "{p:?}");
            assert!(latest_platform(&t.path().join("nope")).is_none());
        }

        #[test]
        fn availability_matches_lookup() {
            assert_eq!(available(), find_mpcmdrun().is_some());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes() {
        assert_eq!(interpret(Some(0), "Scan finished"), Ok(ScanOutcome::Clean));
        match interpret(Some(2), "Scanning x\nFound 1 threats.\n\n  Threat: EICAR\n") {
            Ok(ScanOutcome::Threat(s)) => assert!(s.contains("EICAR")),
            other => panic!("{other:?}"),
        }
        assert!(interpret(Some(1), "boom").unwrap_err().contains("exit code 1"));
        assert!(interpret(None, "").is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn unavailable_off_windows() {
        assert!(!available());
        assert!(scan_file(Path::new("x.zip")).is_err());
    }
}
