//! OS helpers: open a file with its default application, detect the system colour scheme.

use std::path::Path;

/// Open `p` with the system default application.
pub fn open_path(p: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "shell32")]
        unsafe extern "system" {
            fn ShellExecuteW(
                hwnd: *mut std::ffi::c_void,
                operation: *const u16,
                file: *const u16,
                parameters: *const u16,
                directory: *const u16,
                show: i32,
            ) -> isize;
        }

        let path: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: all pointers are null or point to a NUL-terminated UTF-16 buffer that
        // outlives the call.
        let result = unsafe { ShellExecuteW(std::ptr::null_mut(), std::ptr::null(), path.as_ptr(), std::ptr::null(), std::ptr::null(), 1) };
        if result <= 32 {
            return Err(format!("無法開啟 {}（Windows 錯誤碼：{}）", p.display(), result));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let prog = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        std::process::Command::new(prog).arg(p).spawn().map(|_| ()).map_err(|e| format!("無法開啟 {}：{e}", p.display()))
    }
}

/// The user prefers a dark colour scheme (Windows: AppsUseLightTheme = 0).
/// `ARCHIVER_THEME=dark|light` overrides the detection.
pub fn prefers_dark() -> bool {
    match std::env::var("ARCHIVER_THEME").ok().as_deref() {
        Some("dark") => return true,
        Some("light") => return false,
        _ => {}
    }
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
            .and_then(|k| k.get_value::<u32, _>("AppsUseLightTheme"))
            .is_ok_and(|v| v == 0)
    }
    #[cfg(not(windows))]
    {
        false
    }
}
