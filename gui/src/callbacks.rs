use super::*;

pub(super) fn wire_callbacks(ui: &AppWindow, st: &S) {
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_action(move |a| {
            if let Some(ui) = w.upgrade() {
                do_action(&ui, &st, &a);
            }
        });
    }
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_row_clicked(move |i, ctrl, shift| {
            let Some(ui) = w.upgrade() else { return };
            let i = i as usize;
            {
                let mut s = st.borrow_mut();
                if i >= s.items.len() {
                    return;
                }
                s.status.clear();
                if shift {
                    let a = s.anchor.unwrap_or(i);
                    let (lo, hi) = (a.min(i), a.max(i));
                    if !ctrl {
                        s.selected.iter_mut().for_each(|x| *x = false);
                    }
                    for k in lo..=hi {
                        s.selected[k] = true;
                    }
                } else if ctrl {
                    s.selected[i] = !s.selected[i];
                    s.anchor = Some(i);
                } else {
                    s.selected.iter_mut().for_each(|x| *x = false);
                    s.selected[i] = true;
                    s.anchor = Some(i);
                }
            }
            sync_selection(&ui, &st);
            selection_changed(&ui, &st);
        });
    }
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_row_context_requested(move |i| {
            let Some(ui) = w.upgrade() else { return false };
            if i < 0 || is_busy(&ui, &st) {
                return false;
            }
            {
                let mut s = st.borrow_mut();
                let State { items, selected, .. } = &mut *s;
                if !view::select_context_item(items, selected, i as usize) {
                    return false;
                }
                s.anchor = Some(i as usize);
                s.status.clear();
            }
            sync_selection(&ui, &st);
            selection_changed(&ui, &st);
            ui.get_can_extract_selection()
        });
    }
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_row_double_clicked(move |i| {
            if let Some(ui) = w.upgrade() {
                activate(&ui, &st, i as usize);
            }
        });
    }
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_sort(move |c| {
            let Some(ui) = w.upgrade() else { return };
            {
                let mut s = st.borrow_mut();
                if s.sort_col == c {
                    s.sort_asc = !s.sort_asc;
                } else {
                    s.sort_col = c;
                    s.sort_asc = true;
                }
            }
            refresh(&ui, &st);
        });
    }
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_format_changed(move |i| {
            if let Some(ui) = w.upgrade() {
                apply_format(&ui, &st, i.max(0) as usize);
            }
        });
    }
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_shell_toggled(move |on| {
            let Some(ui) = w.upgrade() else { return };
            let r = if on { shell::register() } else { shell::unregister() };
            let msg = match r {
                Ok(m) => m,
                Err(e) => format!("錯誤：{e}"),
            };
            ui.set_shell_enabled(shell::is_registered());
            set_status(&ui, &st, msg);
        });
    }
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_key(move |k| {
            let Some(ui) = w.upgrade() else { return };
            match k.as_str() {
                "enter" => {
                    let idx = st.borrow().selected.iter().position(|&x| x);
                    if let Some(i) = idx {
                        activate(&ui, &st, i);
                    }
                }
                "up" => go_up(&ui, &st),
                "prev" => move_selection(&ui, &st, -1),
                "next" => move_selection(&ui, &st, 1),
                "delete" => {
                    if ui.get_can_delete() {
                        do_delete(&ui, &st);
                    }
                }
                _ => {}
            }
        });
    }
}
