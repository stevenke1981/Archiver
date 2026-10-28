use super::*;

// ------------------------------------------------------------------ OS helpers

pub(super) fn open_path(p: &Path) {
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
        let result = unsafe {
            ShellExecuteW(std::ptr::null_mut(), std::ptr::null(), path.as_ptr(),
                std::ptr::null(), std::ptr::null(), 1)
        };
        if result <= 32 {
            error_box(&format!("無法開啟 {}（Windows 錯誤碼：{}）", p.display(), result));
        }
    }
    #[cfg(not(windows))]
    {
        let mut cmd = if cfg!(target_os = "macos") {
            let mut c = std::process::Command::new("open");
            c.arg(p);
            c
        } else {
            let mut c = std::process::Command::new("xdg-open");
            c.arg(p);
            c
        };
        if let Err(e) = cmd.spawn() {
            error_box(&format!("無法開啟 {}：{e}", p.display()));
        }
    }
}

pub(super) fn confirm(title: &str, text: &str) -> bool {
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(text)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}

pub(super) fn info_box(title: &str, text: &str) {
    rfd::MessageDialog::new().set_title(title).set_description(text).set_level(rfd::MessageLevel::Info).show();
}

pub(super) fn error_box(text: &str) {
    rfd::MessageDialog::new().set_title("Archiver - 錯誤").set_description(text).set_level(rfd::MessageLevel::Error).show();
}
