//! Virus scan of an archive via Microsoft Defender (MpCmdRun.exe) on Windows.
//! CONTRACT STUB (workstream B implements).

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanOutcome {
    Clean,
    /// Defender reported a threat; text is the scanner output summary.
    Threat(String),
}

/// Whether a scanner is available on this system (false on non-Windows).
pub fn available() -> bool {
    false
}

/// Blocking scan of one file.
pub fn scan_file(path: &Path) -> Result<ScanOutcome, String> {
    let _ = path;
    Err("virus scan is not available".into())
}
