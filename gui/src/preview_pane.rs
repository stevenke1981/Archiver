use super::*;

// ------------------------------------------------------------------ preview pane

pub(super) fn set_preview(ui: &AppWindow, mode: &str, title: &str, meta: &str, text: &str) {
    ui.set_preview_mode(mode.into());
    ui.set_preview_title(title.into());
    ui.set_preview_meta(meta.into());
    ui.set_preview_text(text.into());
}

/// "12,345 位元組　2026/10/10 12:00　CRC32 0000ABCD"
pub(super) fn item_meta(it: &Item) -> String {
    let mut v = Vec::new();
    if let Some(n) = it.size {
        v.push(format!("{} 位元組", view::fmt_num(n)));
    }
    if !it.modified.is_empty() {
        v.push(it.modified.replace('-', "/"));
    }
    if let Some(c) = it.crc {
        v.push(format!("CRC32 {c:08X}"));
    }
    v.join("　")
}

/// Contents of the archive folder `dir` (entry name without trailing '/').
pub(super) fn folder_summary(entries: &[Entry], dir: &str) -> String {
    let prefix = format!("{}/", dir.trim_end_matches('/'));
    let (mut files, mut bytes, mut dirs) = (0usize, 0u64, BTreeSet::new());
    for e in entries {
        let name = e.name.replace('\\', "/");
        let name = name.trim_start_matches("./").trim_start_matches('/');
        let Some(rest) = name.strip_prefix(&prefix) else { continue };
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() {
            continue;
        }
        for (i, c) in rest.char_indices() {
            if c == '/' {
                dirs.insert(rest[..i].to_string());
            }
        }
        if e.is_dir {
            dirs.insert(rest.to_string());
        } else {
            files += 1;
            bytes += e.size;
        }
    }
    format!("包含 {files} 個檔案、{} 個資料夾\n共 {} 個位元組", dirs.len(), view::fmt_num(bytes))
}

/// Selection changed (or the pane was toggled / the listing reloaded): cancel the running
/// preview and show the new one (files are read after a short debounce).
pub(super) fn selection_changed(ui: &AppWindow, st: &S) {
    let mut s = st.borrow_mut();
    s.pv.generation += 1;
    s.pv.cancel.store(true, Ordering::Relaxed);
    s.pv.timer.stop();
    if !ui.get_preview_visible() {
        return;
    }
    ui.set_preview_image(slint::Image::default());
    let sel = selected_items(&s);
    let it = match sel.as_slice() {
        [] => return set_preview(ui, "none", "", "", ""),
        [it] => it.clone(),
        many => {
            let bytes: u64 = many.iter().filter_map(|i| i.size).sum();
            return set_preview(ui, "info", &format!("已選取 {} 個項目", many.len()), "", &format!("共 {} 個位元組", view::fmt_num(bytes)));
        }
    };
    let meta = item_meta(&it);
    if it.is_dir {
        let text = match &s.mode {
            Mode::Archive { entries, .. } => folder_summary(entries, &it.full),
            Mode::Fs(_) => "檔案資料夾（按兩下開啟）".to_string(),
        };
        return set_preview(ui, "folder", &it.name, &meta, &text);
    }
    set_preview(ui, "loading", &it.name, &meta, "");
    let generation = s.pv.generation;
    let (w, weak) = (ui.as_weak(), Rc::downgrade(st));
    s.pv.timer.start(slint::TimerMode::SingleShot, PREVIEW_DEBOUNCE, move || {
        if let Some(st) = weak.upgrade()
            && w.upgrade().is_some()
        {
            start_preview(&st, generation, &it);
        }
    });
}

pub(super) fn start_preview(st: &S, generation: u64, it: &Item) {
    let mut s = st.borrow_mut();
    if s.pv.generation != generation {
        return;
    }
    let src = match &s.mode {
        Mode::Archive { path, .. } => PvSrc::Entry { archive: path.clone(), name: it.full.clone(), password: opt_pw(&s) },
        Mode::Fs(_) => PvSrc::File(PathBuf::from(&it.full)),
    };
    let cancel = Arc::new(AtomicBool::new(false));
    s.pv.cancel = cancel.clone();
    let (tx, name) = (s.pv.tx.clone(), it.name.clone());
    std::thread::spawn(move || {
        let _ = tx.send(preview_worker(generation, src, &name, cancel));
    });
}

/// First `max` bytes of a file and whether it is longer.
pub(super) fn read_head(path: &Path, max: usize) -> std::io::Result<(Vec<u8>, bool)> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(max as u64 + 1).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > max;
    bytes.truncate(max);
    Ok((bytes, truncated))
}

/// Background part of a preview: read the data and classify it.
pub(super) fn preview_worker(generation: u64, src: PvSrc, name: &str, cancel: Arc<AtomicBool>) -> PreviewDone {
    let limit = preview::read_limit(name);
    let read = match &src {
        PvSrc::Entry { archive, name: entry, password } => {
            let noop = |_: u64, _: u64, _: &str| {};
            archiver_core::read_entry(archive, entry, password.as_deref(), limit, &Progress { on: &noop, cancel }).map(|d| (d.bytes, d.truncated)).map_err(|e| e.to_string())
        }
        PvSrc::File(p) => read_head(p, limit).map_err(|e| e.to_string()),
    };
    let (bytes, truncated) = match read {
        Ok(r) => r,
        Err(e) => return PreviewDone { generation, body: PvBody::Error(e), full: None },
    };
    let body = match preview::classify(name, &bytes, truncated) {
        Preview::Text(t) => PvBody::Text(t),
        Preview::Hex(h) => PvBody::Hex(h),
        Preview::Empty => PvBody::Empty,
        Preview::Image(ImageKind::Svg) => PvBody::Svg(bytes.clone(), preview::hex_dump(&bytes, HEX_FALLBACK)),
        Preview::Image(ImageKind::Raster) => {
            let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "img".into());
            let dir = view::temp_view_dir().join("preview");
            let file = dir.join(format!("{generation}.{ext}"));
            match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&file, &bytes)) {
                Ok(()) => PvBody::Raster(file, preview::hex_dump(&bytes, HEX_FALLBACK)),
                Err(_) => PvBody::Hex(preview::hex_dump(&bytes, HEX_FALLBACK)),
            }
        }
    };
    let full = match src {
        PvSrc::Entry { archive, name, .. } if !truncated => Some((archive, name, bytes)),
        _ => None,
    };
    PreviewDone { generation, body, full }
}

/// Apply the finished preview of the current generation (called from the poll timer).
pub(super) fn poll_preview(ui: &AppWindow, st: &S) {
    let mut latest = None;
    {
        let s = st.borrow();
        while let Ok(d) = s.pv.rx.try_recv() {
            if d.generation == s.pv.generation {
                latest = Some(d);
            } else if let PvBody::Raster(p, _) = &d.body {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    let Some(d) = latest else { return };
    if d.full.is_some() {
        st.borrow_mut().pv.cache = d.full;
    }
    let (mode, text) = match d.body {
        PvBody::Text(t) => ("text", t),
        PvBody::Hex(h) => ("hex", h),
        PvBody::Empty => ("info", "（空白檔案，0 位元組）".to_string()),
        PvBody::Raster(p, fallback) => {
            let img = slint::Image::load_from_path(&p);
            let _ = std::fs::remove_file(&p); // already decoded into memory
            match img {
                Ok(img) => {
                    ui.set_preview_image(img);
                    ("image", String::new())
                }
                Err(_) => ("hex", fallback),
            }
        }
        PvBody::Svg(data, fallback) => match slint::Image::load_from_svg_data(&data) {
            Ok(img) => {
                ui.set_preview_image(img);
                ("image", String::new())
            }
            Err(_) => ("hex", fallback),
        },
        PvBody::Error(e) => ("error", if is_password_err(&e) { PREVIEW_PW_MSG.to_string() } else { format!("無法預覽：{e}") }),
    };
    ui.set_preview_text(text.into());
    ui.set_preview_mode(mode.into());
}

pub(super) fn do_test(ui: &AppWindow, st: &S) {
    let (targets, pw) = {
        let s = st.borrow();
        let t: Vec<PathBuf> = match &s.mode {
            Mode::Archive { path, .. } => vec![path.clone()],
            Mode::Fs(_) => selected_items(&s).into_iter().filter(|i| Format::detect(Path::new(&i.name)).is_some()).map(|i| PathBuf::from(i.full)).collect(),
        };
        (t, opt_pw(&s))
    };
    if targets.is_empty() {
        return set_status(ui, st, "請先開啟或選取壓縮檔");
    }
    spawn(ui, st, "測試中…", move |p| {
        let mut lines = Vec::new();
        for a in &targets {
            match archiver_core::test(a, pw.as_deref(), p) {
                Ok(n) => lines.push(format!("{}：{n} 個檔案測試正確，沒有發現錯誤。", a.file_name().unwrap_or_default().to_string_lossy())),
                Err(e) => return Msg::Done(Err(format!("{}：{e}", a.display())), After::Nothing),
            }
        }
        Msg::Done(Ok(lines.join("\n")), After::Message)
    });
}

pub(super) fn do_delete(ui: &AppWindow, st: &S) {
    let (arc, names, pw) = {
        let s = st.borrow();
        let Mode::Archive { path, .. } = &s.mode else { return };
        let names: Vec<String> = selected_items(&s).into_iter().map(|i| i.full).collect();
        (path.clone(), names, opt_pw(&s))
    };
    if names.is_empty() {
        return set_status(ui, st, "請先選取要刪除的檔案");
    }
    if !confirm("刪除檔案", &format!("確定要從壓縮檔中刪除 {} 個項目嗎？", names.len())) {
        return;
    }
    spawn(ui, st, "刪除中…", move |p| {
        let r = archiver_core::delete(&arc, &names, pw.as_deref(), p).map(|_| format!("已刪除 {} 個項目", names.len())).map_err(|e| e.to_string());
        Msg::Done(r, After::ReloadArchive)
    });
}

pub(super) fn info_text(s: &State) -> String {
    match &s.mode {
        Mode::Archive { path, fmt, entries, .. } => {
            let files = entries.iter().filter(|e| !e.is_dir).count();
            let dirs = entries.iter().filter(|e| e.is_dir).count();
            let unpacked: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
            let packed = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let ratio = if unpacked > 0 { packed as f64 * 100.0 / unpacked as f64 } else { 0.0 };
            let enc = entries.iter().any(|e| e.encrypted) || !s.password.is_empty();
            format!(
                "壓縮檔：{}\n格式：{}\n檔案：{}　資料夾：{}\n未封裝大小：{} 位元組\n壓縮檔大小：{} 位元組\n壓縮率：{:.1}%\n加密：{}",
                path.display(),
                fmt.label(),
                files,
                dirs,
                view::fmt_num(unpacked),
                view::fmt_num(packed),
                ratio,
                if enc { "是" } else { "否" }
            )
        }
        Mode::Fs(dir) => {
            let files = s.items.iter().filter(|i| !i.is_dir).count();
            let total: u64 = s.items.iter().filter_map(|i| i.size).sum();
            format!("資料夾：{}\n檔案：{files}\n大小：{} 位元組\n\n開啟壓縮檔後可查看壓縮檔資訊。", dir.display(), view::fmt_num(total))
        }
    }
}

pub(super) fn handle_drops(ui: &AppWindow, st: &S, drops: Vec<PathBuf>) {
    if st.borrow().job.is_some() {
        return;
    }
    if drops.len() == 1 && drops[0].is_file() && Format::detect(&drops[0]).is_some() && ui.get_dialog() != "create" {
        open_archive(ui, st, drops[0].clone());
    } else {
        open_create(ui, st, drops, None);
    }
}

pub(super) fn install_drop_handler(ui: &AppWindow, st: &S) {
    use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
    let st = st.clone();
    ui.window().on_winit_window_event(move |_, ev| {
        if let winit::event::WindowEvent::DroppedFile(p) = ev {
            st.borrow_mut().drops.push(p.clone());
        }
        EventResult::Propagate
    });
}
