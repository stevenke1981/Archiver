use super::*;

// ------------------------------------------------------------------ clipboard

pub(super) fn copy_to_clipboard(ui: &AppWindow, st: &S, paths: &[PathBuf]) {
    let r = st.borrow_mut().clip.copy_files(paths);
    match r {
        Ok(()) => {
            // the previous staging copy is no longer on the clipboard: free its disk space now
            let staged = paths.first().and_then(|p| view::clip_dir_of(p));
            let old = std::mem::replace(&mut st.borrow_mut().last_clip, staged.clone());
            if let Some(old) = old.filter(|o| Some(o) != staged.as_ref()) {
                std::thread::spawn(move || std::fs::remove_dir_all(old));
            }
            set_status(ui, st, format!("已複製 {} 個項目到剪貼簿，可在檔案總管中貼上", paths.len()))
        }
        Err(e) => {
            set_status(ui, st, format!("錯誤：{e}"));
            error_box(&e);
        }
    }
}

/// Write the cached bytes of a single archive entry into a fresh clipboard staging folder.
pub(super) fn stage_bytes(bytes: &[u8], items: &[Item], single_file: bool) -> std::io::Result<Vec<PathBuf>> {
    let dest = view::new_clip_dir()?;
    let paths = view::extracted_paths(&dest, items, single_file);
    let r = (|| {
        let target = paths.first().ok_or_else(|| std::io::Error::other("no target path"))?;
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(target, bytes)
    })();
    if r.is_err() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    r.map(|()| paths)
}

/// Copy the selected items to the clipboard as files (archive items are extracted first).
pub(super) fn do_copy(ui: &AppWindow, st: &S) {
    let (sel, arc, pw) = {
        let s = st.borrow();
        let arc = match &s.mode {
            Mode::Archive { path, fmt, .. } => Some((path.clone(), *fmt)),
            Mode::Fs(_) => None,
        };
        (selected_items(&s), arc, opt_pw(&s))
    };
    if sel.is_empty() {
        return set_status(ui, st, "請先選取要複製的檔案");
    }
    let Some((arc, fmt)) = arc else {
        let paths: Vec<PathBuf> = sel.iter().map(|i| PathBuf::from(&i.full)).collect();
        return copy_to_clipboard(ui, st, &paths);
    };
    // quick path: the preview already holds the complete entry
    if let [it] = sel.as_slice()
        && !it.is_dir
    {
        let staged = {
            let s = st.borrow();
            s.pv.cache.as_ref().filter(|(a, n, _)| *a == arc && *n == it.full).map(|(_, _, b)| stage_bytes(b, &sel, fmt.is_single_file()))
        };
        if let Some(Ok(paths)) = staged {
            return copy_to_clipboard(ui, st, &paths);
        }
    }
    let names: Vec<String> = sel.iter().map(|i| i.full.clone()).collect();
    spawn(ui, st, "複製到剪貼簿…", move |p| {
        let dest = match view::new_clip_dir() {
            Ok(d) => d,
            Err(e) => return Msg::Done(Err(e.to_string()), After::Nothing),
        };
        let r = archiver_core::extract(&arc, &dest, pw.as_deref(), Some(&names), p).map_err(|e| e.to_string()).and_then(|_| {
            // only what was really written (entries core refused are not on disk)
            let paths: Vec<PathBuf> = view::extracted_paths(&dest, &sel, fmt.is_single_file()).into_iter().filter(|p| p.exists()).collect();
            if paths.is_empty() { Err("沒有可複製的檔案".to_string()) } else { Ok(paths) }
        });
        match r {
            Ok(paths) => Msg::Done(Ok(String::new()), After::CopyFiles(paths)),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dest);
                Msg::Done(Err(e), After::Nothing)
            }
        }
    });
}
