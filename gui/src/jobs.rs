use super::*;

// ------------------------------------------------------------------ background jobs

pub(super) fn spawn<F>(ui: &AppWindow, st: &S, title: &str, f: F)
where
    F: FnOnce(&Progress) -> Msg + Send + 'static,
{
    if is_busy(ui, st) {
        return;
    }
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(Mutex::new((0u64, 0u64, String::new())));
    let (c2, p2) = (cancel.clone(), progress.clone());
    std::thread::spawn(move || {
        let cb = move |d: u64, t: u64, n: &str| {
            if let Ok(mut g) = p2.lock() {
                *g = (d, t, n.to_string());
            }
        };
        let prog = Progress { on: &cb, cancel: c2 };
        let _ = tx.send(f(&prog));
    });
    st.borrow_mut().job = Some(Job { rx, cancel, progress, title: title.into() });
    ui.set_busy(true);
    ui.set_progress(0.0);
    ui.set_progress_text(title.into());
}

/// True (and a status hint) while a background job runs.
pub(super) fn is_busy(ui: &AppWindow, st: &S) -> bool {
    let busy = st.borrow().job.is_some();
    if busy {
        set_status(ui, st, BUSY_MSG);
    }
    busy
}

pub(super) fn poll(ui: &AppWindow, st: &S) {
    poll_preview(ui, st);
    let drops = std::mem::take(&mut st.borrow_mut().drops);
    if !drops.is_empty() {
        handle_drops(ui, st, drops);
    }
    let msg = {
        let s = st.borrow();
        let Some(job) = &s.job else { return };
        let (d, t, n) = job.progress.lock().map(|g| g.clone()).unwrap_or_default();
        ui.set_progress(if t > 0 { (d as f32 / t as f32).min(1.0) } else { 0.0 });
        ui.set_progress_text(if n.is_empty() { job.title.clone() } else { format!("{} {n}", job.title) }.into());
        match job.rx.try_recv() {
            Ok(m) => m,
            Err(_) => return,
        }
    };
    st.borrow_mut().job = None;
    ui.set_busy(false);
    match msg {
        Msg::Listed(path, Ok(entries)) => {
            let fmt = Format::detect(&path).unwrap_or(Format::Zip);
            {
                let mut s = st.borrow_mut();
                // re-listing the open archive (F5, after delete) keeps the folder and status
                let prefix = match &s.mode {
                    Mode::Archive { path: p, prefix, .. } if *p == path => Some(prefix.clone()),
                    _ => None,
                };
                if prefix.is_none() {
                    s.filter.clear();
                    s.status.clear();
                }
                s.mode = Mode::Archive { path, fmt, entries, prefix: prefix.unwrap_or_default() };
                s.pv.cache = None;
            }
            refresh(ui, st);
        }
        Msg::Listed(path, Err(e)) => {
            set_status(ui, st, format!("無法開啟 {}: {e}", path.display()));
            if e.to_lowercase().contains("password") {
                ui.set_archive_password(SharedString::new());
                ui.set_dialog("password".into());
                st.borrow_mut().fallback_open = Some(path);
            }
        }
        Msg::Done(Ok(text), after) => {
            st.borrow_mut().status = text.clone();
            let batch_failed = matches!(after, After::Batch { failed, .. } if failed > 0);
            if st.borrow().close_when_done && !batch_failed {
                let _ = ui.hide();
                let _ = slint::quit_event_loop();
                return;
            }
            match after {
                After::Nothing => update_status(ui, &st.borrow()),
                After::ReloadFs => {
                    if matches!(st.borrow().mode, Mode::Fs(_)) {
                        refresh(ui, st);
                    }
                    update_status(ui, &st.borrow());
                }
                After::ReloadArchive => {
                    let path = match &st.borrow().mode {
                        Mode::Archive { path, .. } => Some(path.clone()),
                        _ => None,
                    };
                    if let Some(p) = path {
                        reload_archive_keep_prefix(ui, st, p);
                    }
                }
                After::OpenFile(p) => {
                    open_path(&p);
                    update_status(ui, &st.borrow());
                }
                After::Message => {
                    update_status(ui, &st.borrow());
                    info_box("Archiver", &text);
                }
                After::CopyFiles(paths) => copy_to_clipboard(ui, st, &paths),
                After::Batch { failed, retry } => {
                    let from_cli = {
                        let mut s = st.borrow_mut();
                        s.status = text.lines().next().unwrap_or_default().to_string();
                        std::mem::take(&mut s.close_when_done)
                    };
                    if matches!(st.borrow().mode, Mode::Fs(_)) {
                        refresh(ui, st);
                    }
                    update_status(ui, &st.borrow());
                    match retry.filter(|_| from_cli && failed == 1) {
                        // e.g. encrypted archive from the context menu: open it so a password can be entered
                        Some(p) => open_archive(ui, st, p),
                        None if failed > 0 => error_box(&text),
                        None => {}
                    }
                }
            }
        }
        Msg::Done(Err(e), _) => {
            let fallback = {
                let mut s = st.borrow_mut();
                s.close_when_done = false;
                s.fallback_open.take()
            };
            set_status(ui, st, format!("錯誤：{e}"));
            if let Some(p) = fallback {
                // e.g. encrypted archive from the context menu: open it so a password can be entered
                open_archive(ui, st, p);
            } else {
                error_box(&e);
            }
        }
    }
}

pub(super) fn reload_archive_keep_prefix(ui: &AppWindow, st: &S, path: PathBuf) {
    // listing can take long (.tar.gz is read in full): in the background, like open_archive
    let pw = opt_pw(&st.borrow());
    spawn(ui, st, "讀取中…", move |_| {
        let r = archiver_core::list(&path, pw.as_deref()).map_err(|e| e.to_string());
        Msg::Listed(path, r)
    });
}
