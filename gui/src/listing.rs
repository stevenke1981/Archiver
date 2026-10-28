use super::*;

// ------------------------------------------------------------------ listing / refresh

pub(super) fn refresh(ui: &AppWindow, st: &S) {
    let mut s = st.borrow_mut();
    let mut items = match &s.mode {
        Mode::Fs(dir) => {
            let mut v = view::fs_children(dir).unwrap_or_default();
            if !s.filter.is_empty() {
                let f = s.filter.to_lowercase();
                v.retain(|i| i.name.to_lowercase().contains(&f));
            }
            if dir.parent().is_some() {
                v.insert(0, Item::parent());
            }
            v
        }
        Mode::Archive { entries, prefix, .. } => {
            let mut v = if s.filter.is_empty() { view::archive_children(entries, prefix) } else { view::archive_search(entries, &s.filter) };
            v.insert(0, Item::parent());
            v
        }
    };
    view::sort_items(&mut items, s.sort_col, s.sort_asc);
    s.selected = vec![false; items.len()];
    s.anchor = None;
    s.items = items;
    let (title, path_text, archive_open) = match &s.mode {
        Mode::Fs(dir) => ("Archiver".to_string(), dir.display().to_string(), false),
        Mode::Archive { path, fmt, entries, prefix } => {
            let unpacked: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
            let label = fmt.label().split(' ').next().unwrap_or("").to_string();
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let mut pt = view::archive_path_text(path, prefix, &label, unpacked);
            if !s.filter.is_empty() {
                pt = format!("{pt}  （尋找：{}）", s.filter);
            }
            (format!("{name} - Archiver"), pt, true)
        }
    };
    ui.set_window_title(title.into());
    ui.set_path_text(path_text.into());
    ui.set_archive_open(archive_open);
    ui.set_can_delete(archive_open && matches!(&s.mode, Mode::Archive { fmt, .. } if fmt.can_create() && !fmt.is_single_file()));
    ui.set_sort_column(s.sort_col);
    ui.set_sort_ascending(s.sort_asc);
    drop(s);
    push_rows(ui, st);
    selection_changed(ui, st);
}

pub(super) fn push_rows(ui: &AppWindow, st: &S) {
    let s = st.borrow();
    let in_archive = matches!(s.mode, Mode::Archive { .. });
    let rows: Vec<Row> = s
        .items
        .iter()
        .zip(&s.selected)
        .map(|(it, &sel)| Row {
            name: it.name.clone().into(),
            size: it.size.map(view::fmt_num).unwrap_or_default().into(),
            packed: if in_archive { it.packed.map(view::fmt_num).unwrap_or_default() } else { String::new() }.into(),
            kind: if it.is_parent { "檔案資料夾".to_string() } else { view::type_name(&it.name, it.is_dir) }.into(),
            modified: it.modified.replace('-', "/").into(),
            crc: it.crc.map(|c| format!("{c:08X}")).unwrap_or_default().into(),
            icon: view::icon(it),
            selected: sel,
        })
        .collect();
    ui.set_rows(ModelRc::new(VecModel::from(rows)));
    update_status(ui, &s);
}

pub(super) fn update_status(ui: &AppWindow, s: &State) {
    ui.set_can_extract_selection(view::can_extract_selection(matches!(s.mode, Mode::Archive { .. }), &s.items, &s.selected));
    let files: Vec<&Item> = s.items.iter().filter(|i| !i.is_dir).collect();
    let total: u64 = files.iter().filter_map(|i| i.size).sum();
    let dirs = s.items.iter().filter(|i| i.is_dir && !i.is_parent).count();
    let mut right = format!("總共 {} 個檔案, {} 個位元組", files.len(), view::fmt_num(total));
    if dirs > 0 {
        right = format!("總共 {dirs} 個資料夾和 {} 個檔案, {} 個位元組", files.len(), view::fmt_num(total));
    }
    let sel: Vec<&Item> = s.items.iter().zip(&s.selected).filter(|(i, x)| **x && !i.is_parent).map(|(i, _)| i).collect();
    let left = if !s.status.is_empty() {
        s.status.clone()
    } else if sel.is_empty() {
        String::new()
    } else {
        let b: u64 = sel.iter().filter_map(|i| i.size).sum();
        format!("已選取 {} 個項目, {} 個位元組", sel.len(), view::fmt_num(b))
    };
    ui.set_status_left(left.into());
    ui.set_status_right(right.into());
}

pub(super) fn set_status(ui: &AppWindow, st: &S, msg: impl Into<String>) {
    st.borrow_mut().status = msg.into();
    update_status(ui, &st.borrow());
}

pub(super) fn selected_items(st: &State) -> Vec<Item> {
    st.items.iter().zip(&st.selected).filter(|(i, x)| **x && !i.is_parent).map(|(i, _)| i.clone()).collect()
}

/// Update only the selection flags in the existing row model.
pub(super) fn sync_selection(ui: &AppWindow, st: &S) {
    let rows = ui.get_rows();
    let s = st.borrow();
    for (k, &sel) in s.selected.iter().enumerate() {
        if let Some(mut r) = rows.row_data(k)
            && r.selected != sel
        {
            r.selected = sel;
            rows.set_row_data(k, r);
        }
    }
    update_status(ui, &s);
}

/// Up/Down arrow: move a single selection by `delta` rows.
pub(super) fn move_selection(ui: &AppWindow, st: &S, delta: i32) {
    let next = {
        let mut s = st.borrow_mut();
        let n = s.items.len();
        if n == 0 {
            return;
        }
        let cur = s.anchor.or_else(|| s.selected.iter().position(|&x| x));
        let next = cur.map_or(0, |c| (c as i64 + delta as i64).clamp(0, n as i64 - 1) as usize);
        s.selected.iter_mut().for_each(|x| *x = false);
        s.selected[next] = true;
        s.anchor = Some(next);
        s.status.clear();
        next
    };
    sync_selection(ui, st);
    ui.invoke_ensure_row_visible(next as i32);
    selection_changed(ui, st);
}
