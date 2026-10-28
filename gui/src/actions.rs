use super::*;

// ------------------------------------------------------------------ actions

pub(super) fn open_archive(ui: &AppWindow, st: &S, path: PathBuf) {
    if Format::detect(&path).is_none() {
        set_status(ui, st, format!("不支援的格式：{}", path.display()));
        return;
    }
    let pw = opt_pw(&st.borrow());
    spawn(ui, st, "讀取中…", move |_| {
        let r = archiver_core::list(&path, pw.as_deref()).map_err(|e| e.to_string());
        Msg::Listed(path, r)
    });
}

/// Context-menu "extract here / to folder": extract every archive, continue after failures.
pub(super) fn extract_batch(ui: &AppWindow, st: &S, files: Vec<PathBuf>, to_folder: bool) {
    st.borrow_mut().close_when_done = true;
    let dest = if to_folder { BatchDest::BesideInFolder } else { BatchDest::Beside };
    spawn(ui, st, "解壓縮中…", move |p| batch_msg(&archiver_core::extract_each(&files, &dest, None, p)));
}

/// Shell command: let the user choose a destination; one archive goes directly into it,
/// multiple archives keep the manager's existing per-archive folder behavior.
pub(super) fn extract_to(ui: &AppWindow, st: &S, files: Vec<PathBuf>) {
    if is_busy(ui, st) || files.is_empty() {
        return;
    }
    let start = files[0].parent().unwrap_or(Path::new(""));
    let Some(dest) = rfd::FileDialog::new().set_title("解壓縮至…").set_directory(start).pick_folder() else { return };
    {
        let mut s = st.borrow_mut();
        s.close_when_done = true;
        if files.len() == 1 {
            s.fallback_open = Some(files[0].clone());
        }
    }
    extract_into(ui, st, files, None, None, dest);
}

pub(super) fn is_password_err(e: &str) -> bool {
    e.to_lowercase().contains("password")
}

/// "已完成 N 個（成功 X，失敗 Y）" plus one line per failure (at most 20); returns the failure count.
pub(super) fn batch_summary(results: &[BatchResult]) -> (String, usize) {
    const MAX_LINES: usize = 20;
    let failed: Vec<(&BatchResult, String)> = results.iter().filter_map(|r| r.result.as_ref().err().map(|e| (r, e.to_string()))).collect();
    let mut text = format!("已完成 {} 個（成功 {}，失敗 {}）", results.len(), results.len() - failed.len(), failed.len());
    for (r, e) in failed.iter().take(MAX_LINES) {
        let name = r.input.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| r.input.display().to_string());
        text.push_str(&format!("\n✗ {name}：{e}"));
    }
    if failed.len() > MAX_LINES {
        text.push_str(&format!("\n…還有 {} 個", failed.len() - MAX_LINES));
    }
    (text, failed.len())
}

/// The input of the only failed item, if it failed because of a missing/wrong password.
pub(super) fn single_password_failure(results: &[BatchResult]) -> Option<PathBuf> {
    let mut failed = results.iter().filter(|r| r.result.is_err());
    match (failed.next(), failed.next()) {
        (Some(r), None) if r.result.as_ref().is_err_and(|e| is_password_err(&e.to_string())) => Some(r.input.clone()),
        _ => None,
    }
}

pub(super) fn batch_msg(results: &[BatchResult]) -> Msg {
    let (text, failed) = batch_summary(results);
    Msg::Done(Ok(text), After::Batch { failed, retry: single_password_failure(results) })
}

/// Open the create dialog with `files` added. `each`: preset the batch checkbox (a fresh
/// dialog defaults to a single archive).
pub(super) fn open_create(ui: &AppWindow, st: &S, files: Vec<PathBuf>, each: Option<bool>) {
    {
        let mut s = st.borrow_mut();
        for f in files {
            if !s.create_inputs.contains(&f) {
                s.create_inputs.push(f);
            }
        }
    }
    if ui.get_dialog() != "create" {
        ui.set_create_password(SharedString::new());
        ui.set_create_each(each.unwrap_or(false));
        let idx = ui.get_format_index().max(0) as usize;
        apply_format(ui, st, idx);
    } else if let Some(e) = each {
        ui.set_create_each(e);
    }
    update_create_fields(ui, st);
    ui.set_dialog("create".into());
}

pub(super) fn update_create_fields(ui: &AppWindow, st: &S) {
    let s = st.borrow();
    let text = s.create_inputs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
    ui.set_create_sources(text.into());
    let fmt = Format::writable()[ui.get_format_index().max(0) as usize];
    let each = ui.get_create_each();
    let cur = ui.get_create_output();
    let cur = Path::new(cur.trim());
    if let Some(first) = s.create_inputs.first() {
        if !each {
            ui.set_create_output(archiver_core::suggest_output(&s.create_inputs, fmt).display().to_string().into());
        } else if cur.as_os_str().is_empty() || (!cur.is_dir() && Format::detect(cur).is_some()) {
            // empty, or still the single-archive file name: use the first input's folder
            let dir = first.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf).or_else(|| std::env::current_dir().ok()).unwrap_or_default();
            ui.set_create_output(dir.display().to_string().into());
        }
    }
    let warn = if each { s.create_inputs.iter().any(|p| p.is_dir()) } else { s.create_inputs.len() != 1 };
    ui.set_single_file_warning(fmt.is_single_file() && warn);
}

pub(super) fn apply_format(ui: &AppWindow, st: &S, idx: usize) {
    let fmt = Format::writable()[idx.min(Format::writable().len() - 1)];
    ui.set_format_index(idx as i32);
    match fmt.level_range() {
        Some((lo, hi, def)) => {
            ui.set_level_enabled(true);
            ui.set_level_min(lo as f32);
            ui.set_level_max(hi as f32);
            let cur = ui.get_level();
            if cur < lo as f32 || cur > hi as f32 {
                ui.set_level(def as f32);
            }
        }
        None => ui.set_level_enabled(false),
    }
    ui.set_password_enabled(fmt.supports_password());
    update_create_fields(ui, st);
}

pub(super) fn start_create(ui: &AppWindow, st: &S) {
    let fmt = Format::writable()[ui.get_format_index().max(0) as usize];
    let output = PathBuf::from(ui.get_create_output().trim());
    let inputs = st.borrow().create_inputs.clone();
    let each = ui.get_create_each();
    if inputs.is_empty() || output.as_os_str().is_empty() || is_busy(ui, st) {
        return;
    }
    if !each && output.exists() && !confirm("覆寫檔案？", &format!("{} 已存在，要覆寫嗎？", output.display())) {
        return;
    }
    let pw = ui.get_create_password().to_string();
    let opts = CreateOptions {
        format: fmt,
        level: fmt.level_range().map(|_| ui.get_level().round() as u32),
        password: Some(pw).filter(|p| !p.is_empty() && fmt.supports_password()),
    };
    st.borrow_mut().create_inputs.clear();
    ui.set_dialog("".into());
    if each {
        // output is a folder; core picks a unique archive name per input
        return spawn(ui, st, "分批壓縮中…", move |p| {
            if let Err(e) = std::fs::create_dir_all(&output) {
                return Msg::Done(Err(format!("{}: {e}", output.display())), After::Nothing);
            }
            batch_msg(&archiver_core::create_each(&inputs, Some(&output), &opts, p))
        });
    }
    spawn(ui, st, "壓縮中…", move |p| {
        let r = archiver_core::create(&output, &inputs, &opts, p)
            .map(|_| format!("已建立 {}", output.display()))
            .map_err(|e| e.to_string());
        Msg::Done(r, After::ReloadFs)
    });
}

pub(super) fn do_action(ui: &AppWindow, st: &S, a: &str) {
    match a {
        "open" => {
            let exts = archiver_core::archive_extensions();
            if let Some(p) = rfd::FileDialog::new().set_title("開啟壓縮檔").add_filter("壓縮檔", &exts).add_filter("所有檔案", &["*"]).pick_file() {
                open_archive(ui, st, p);
            }
        }
        "add" => do_add(ui, st, None),
        "add-each" => do_add(ui, st, Some(true)),
        "extract" => do_extract(ui, st, ExtractScope::Automatic),
        "extract-selected" => do_extract(ui, st, ExtractScope::Selected),
        "extract-all" => do_extract(ui, st, ExtractScope::All),
        "extract-here" => do_extract_each(ui, st, BatchDest::Beside),
        "extract-each" => do_extract_each(ui, st, BatchDest::BesideInFolder),
        "copy" => do_copy(ui, st),
        "copy-text" => {
            let text = ui.get_preview_text();
            let r = st.borrow_mut().clip.copy_text(&text);
            match r {
                Ok(()) => set_status(ui, st, "已複製文字到剪貼簿"),
                Err(e) => set_status(ui, st, format!("錯誤：{e}")),
            }
        }
        "refresh" => {
            let arc = {
                let mut s = st.borrow_mut();
                s.status.clear();
                match &s.mode {
                    Mode::Archive { path, .. } => Some(path.clone()),
                    Mode::Fs(_) => None,
                }
            };
            match arc {
                Some(p) => reload_archive_keep_prefix(ui, st, p),
                None => refresh(ui, st),
            }
        }
        "preview-changed" => selection_changed(ui, st),
        "create-each-changed" => update_create_fields(ui, st),
        "test" => do_test(ui, st),
        "view" => {
            let first = selected_items(&st.borrow()).into_iter().find(|i| !i.is_dir);
            if let Some(it) = first {
                view_item(ui, st, it);
            }
        }
        "delete" => do_delete(ui, st),
        "find" => {
            ui.set_find_text(st.borrow().filter.clone().into());
            ui.set_dialog("find".into());
        }
        "find-ok" => {
            st.borrow_mut().filter = ui.get_find_text().trim().to_string();
            ui.set_dialog("".into());
            refresh(ui, st);
        }
        "wizard" => {
            if matches!(st.borrow().mode, Mode::Archive { .. }) {
                do_extract(ui, st, ExtractScope::All);
            } else {
                do_action(ui, st, "add");
            }
        }
        "info" => {
            ui.set_info_text(info_text(&st.borrow()).into());
            ui.set_dialog("info".into());
        }
        "shell" => {
            ui.set_shell_enabled(shell::is_registered());
            let d = if cfg!(windows) {
                "Windows 檔案總管右鍵選單（目前使用者 HKCU，不需系統管理員）：開啟、解壓縮至…（選擇目的地）、解壓縮到這裡、解壓縮到資料夾、加入壓縮檔。"
            } else if cfg!(target_os = "macos") {
                "Finder 快速動作（~/Library/Services）。"
            } else {
                "Nautilus 腳本、Dolphin 服務選單、Nemo 動作與 .desktop MIME 關聯。"
            };
            ui.set_shell_description(d.into());
            ui.set_dialog("shell".into());
        }
        "password" => {
            ui.set_archive_password(st.borrow().password.clone().into());
            ui.set_dialog("password".into());
        }
        "password-ok" => {
            st.borrow_mut().password = ui.get_archive_password().to_string();
            ui.set_dialog("".into());
            let target = {
                let mut s = st.borrow_mut();
                match &s.mode {
                    Mode::Archive { path, .. } => Some(path.clone()),
                    _ => s.fallback_open.take(),
                }
            };
            if let Some(p) = target {
                open_archive(ui, st, p);
            }
        }
        "create-browse" if ui.get_create_each() => {
            let cur = PathBuf::from(ui.get_create_output().trim());
            let mut d = rfd::FileDialog::new().set_title("選擇輸出資料夾");
            if cur.is_dir() {
                d = d.set_directory(&cur);
            }
            if let Some(p) = d.pick_folder() {
                ui.set_create_output(p.display().to_string().into());
            }
        }
        "create-browse" => {
            let cur = PathBuf::from(ui.get_create_output().to_string());
            let mut d = rfd::FileDialog::new().set_title("儲存壓縮檔");
            if let Some(dir) = cur.parent().filter(|p| p.is_dir()) {
                d = d.set_directory(dir);
            }
            if let Some(n) = cur.file_name() {
                d = d.set_file_name(n.to_string_lossy());
            }
            if let Some(p) = d.save_file() {
                ui.set_create_output(p.display().to_string().into());
            }
        }
        "create-ok" => start_create(ui, st),
        "dialog-close" => {
            if ui.get_dialog() == "create" {
                st.borrow_mut().create_inputs.clear();
            }
            ui.set_dialog("".into());
        }
        "select-all" => {
            {
                let mut s = st.borrow_mut();
                let n = s.items.len();
                s.selected = (0..n).map(|i| !s.items[i].is_parent).collect();
                s.status.clear();
            }
            push_rows(ui, st);
            selection_changed(ui, st);
        }
        "up" => go_up(ui, st),
        "cancel" => {
            if let Some(j) = &st.borrow().job {
                j.cancel.store(true, Ordering::Relaxed);
            }
        }
        "about" => info_box(
            "關於 Archiver",
            &format!("Archiver {}\n跨平台壓縮／解壓縮工具（Rust + Slint）\nhttps://github.com/stevenke1981/Archiver\nMIT License", env!("CARGO_PKG_VERSION")),
        ),
        "exit" => {
            let _ = slint::quit_event_loop();
        }
        _ => {}
    }
}

pub(super) fn go_up(ui: &AppWindow, st: &S) {
    {
        let mut s = st.borrow_mut();
        s.status.clear();
        if !s.filter.is_empty() {
            s.filter.clear();
        } else {
            let next = match &mut s.mode {
                Mode::Fs(dir) => {
                    if let Some(p) = dir.parent() {
                        *dir = p.to_path_buf();
                    }
                    None
                }
                Mode::Archive { path, prefix, .. } => {
                    if prefix.is_empty() {
                        Some(path.parent().map(Path::to_path_buf).unwrap_or_default())
                    } else {
                        *prefix = view::parent_prefix(prefix);
                        None
                    }
                }
            };
            if let Some(d) = next {
                s.mode = Mode::Fs(d);
                s.pv.cache = None;
            }
        }
    }
    refresh(ui, st);
}

pub(super) fn activate(ui: &AppWindow, st: &S, idx: usize) {
    let Some(it) = st.borrow().items.get(idx).cloned() else { return };
    if it.is_parent {
        return go_up(ui, st);
    }
    let in_archive = matches!(st.borrow().mode, Mode::Archive { .. });
    if it.is_dir {
        {
            let mut s = st.borrow_mut();
            s.filter.clear();
            s.status.clear();
            match &mut s.mode {
                Mode::Fs(dir) => *dir = PathBuf::from(&it.full),
                Mode::Archive { prefix, .. } => *prefix = format!("{}/", it.full),
            }
        }
        return refresh(ui, st);
    }
    if in_archive {
        view_item(ui, st, it);
    } else if Format::detect(Path::new(&it.name)).is_some() {
        open_archive(ui, st, PathBuf::from(it.full));
    } else {
        open_path(Path::new(&it.full));
    }
}

/// Extract one archive entry to a temp folder and open it with the system default app.
pub(super) fn view_item(ui: &AppWindow, st: &S, it: Item) {
    let (arc, pw) = {
        let s = st.borrow();
        match &s.mode {
            Mode::Archive { path, .. } => (path.clone(), opt_pw(&s)),
            Mode::Fs(_) => {
                drop(s);
                return open_path(Path::new(&it.full));
            }
        }
    };
    let dest = view::temp_view_dir();
    spawn(ui, st, "解壓縮檢視中…", move |p| {
        let target = if Format::detect(&arc).is_some_and(|f| f.is_single_file()) { dest.join(&it.name) } else { dest.join(&it.full) };
        match archiver_core::extract(&arc, &dest, pw.as_deref(), Some(&[it.full.clone()]), p) {
            Ok(()) => Msg::Done(Ok(format!("已開啟 {}", it.name)), After::OpenFile(target)),
            Err(e) => Msg::Done(Err(e.to_string()), After::Nothing),
        }
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ExtractScope {
    Automatic,
    Selected,
    All,
}

pub(super) fn extraction_names(items: &[Item], scope: ExtractScope) -> Result<Option<Vec<String>>, &'static str> {
    let names: Vec<String> = items.iter().filter(|i| !i.is_parent).map(|i| i.full.clone()).collect();
    match scope {
        ExtractScope::Selected if names.is_empty() => Err("請先選取要解壓縮的檔案或資料夾"),
        ExtractScope::All => Ok(None),
        _ => Ok((!names.is_empty()).then_some(names)),
    }
}

pub(super) fn do_extract(ui: &AppWindow, st: &S, scope: ExtractScope) {
    if is_busy(ui, st) {
        return;
    }
    if scope == ExtractScope::Selected && !ui.get_can_extract_selection() {
        return set_status(ui, st, "請先選取要解壓縮的檔案或資料夾；檔案系統中請只選取壓縮檔");
    }
    let (sources, sel, start, pw): (Vec<PathBuf>, Option<Vec<String>>, PathBuf, Option<String>) = {
        let s = st.borrow();
        let sel = selected_items(&s);
        match &s.mode {
            Mode::Archive { path, .. } => {
                let names = match extraction_names(&sel, scope) {
                    Ok(names) => names,
                    Err(message) => {
                        drop(s);
                        return set_status(ui, st, message);
                    }
                };
                (vec![path.clone()], names, path.parent().map(Path::to_path_buf).unwrap_or_default(), opt_pw(&s))
            }
            Mode::Fs(dir) => {
                let arcs: Vec<PathBuf> = sel.iter().filter(|i| !i.is_dir && Format::detect(Path::new(&i.name)).is_some()).map(|i| PathBuf::from(&i.full)).collect();
                (arcs, None, dir.clone(), opt_pw(&s))
            }
        }
    };
    if sources.is_empty() {
        let Some(p) = rfd::FileDialog::new().set_title("選擇要解壓縮的壓縮檔").pick_file() else { return };
        return {
            open_archive(ui, st, p);
        };
    }
    let title = if sel.is_some() { "解壓縮選取項目至…" } else { "解壓縮至…" };
    let Some(dest) = rfd::FileDialog::new().set_title(title).set_directory(&start).pick_folder() else { return };
    extract_into(ui, st, sources, sel, pw, dest);
}

pub(super) fn extract_into(ui: &AppWindow, st: &S, sources: Vec<PathBuf>, sel: Option<Vec<String>>, pw: Option<String>, dest: PathBuf) {
    if sources.len() > 1 {
        let dest = BatchDest::IntoFolders(dest);
        return spawn(ui, st, "解壓縮中…", move |p| batch_msg(&archiver_core::extract_each(&sources, &dest, pw.as_deref(), p)));
    }
    spawn(ui, st, "解壓縮中…", move |p| match archiver_core::extract(&sources[0], &dest, pw.as_deref(), sel.as_deref(), p) {
        Ok(()) => Msg::Done(Ok(format!("已解壓縮到 {}", dest.display())), After::ReloadFs),
        Err(e) => Msg::Done(Err(format!("{}: {e}", sources[0].display())), After::Nothing),
    });
}

/// "Extract here" / "extract each to its own folder": the selected archives (file-system
/// mode) or the open archive.
pub(super) fn do_extract_each(ui: &AppWindow, st: &S, dest: BatchDest) {
    let (arcs, pw) = {
        let s = st.borrow();
        let arcs: Vec<PathBuf> = match &s.mode {
            Mode::Archive { path, .. } => vec![path.clone()],
            Mode::Fs(_) => selected_items(&s).into_iter().filter(|i| !i.is_dir && Format::detect(Path::new(&i.name)).is_some()).map(|i| PathBuf::from(i.full)).collect(),
        };
        (arcs, opt_pw(&s))
    };
    if arcs.is_empty() {
        return set_status(ui, st, "請先開啟或選取壓縮檔");
    }
    spawn(ui, st, "解壓縮中…", move |p| batch_msg(&archiver_core::extract_each(&arcs, &dest, pw.as_deref(), p)));
}

pub(super) fn do_add(ui: &AppWindow, st: &S, each: Option<bool>) {
    let fs_sel: Vec<PathBuf> = {
        let s = st.borrow();
        match &s.mode {
            Mode::Fs(_) => selected_items(&s).into_iter().map(|i| PathBuf::from(i.full)).collect(),
            _ => vec![],
        }
    };
    if !fs_sel.is_empty() {
        open_create(ui, st, fs_sel, each);
    } else if let Some(f) = rfd::FileDialog::new().set_title("選擇要加入的檔案").pick_files() {
        open_create(ui, st, f, each);
    }
}
