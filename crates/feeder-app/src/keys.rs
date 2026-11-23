use rune::{Key, KeyEvent, Tx};

use crate::{State, Tab, chrome, feed, home};

pub fn step_selection(ids: &[i64], current: Option<i64>, delta: isize) -> Option<i64> {
    if ids.is_empty() {
        return None;
    }
    let idx = current.and_then(|c| ids.iter().position(|&id| id == c));
    let next = match idx {
        Some(i) => (i as isize + delta).rem_euclid(ids.len() as isize) as usize,
        None if delta >= 0 => 0,
        None => ids.len() - 1,
    };
    Some(ids[next])
}

pub fn on_key(s: &mut State, e: &KeyEvent, tx: &mut Tx) -> bool {
    // ⌘+ / ⌘− / ⌘0 / ⌘9 zoom the Files map while it shows.
    if s.open.is_some() && crate::files::on_key(s, e) {
        return true;
    }
    let nb_editing = s.tab == Tab::Notebook && s.open.is_none() && s.notebook.editor.is_some();
    if nb_editing && !s.palette_open && !s.settings_open {
        if e.is_command('s') {
            s.notebook_save();
            return true;
        }
        if e.mods.cmd && e.mods.shift && matches!(&e.key, Key::Text(t) if t.eq_ignore_ascii_case("p")) {
            if let Some(ed) = s.notebook.editor.as_mut() {
                ed.preview = !ed.preview;
            }
            return true;
        }
        if e.is_command('k') && s.notebook.editor.as_ref().is_some_and(|ed| ed.body.focused) {
            return false;
        }
    }
    if e.is_command('k') {
        if s.palette_open {
            s.close_palette();
        } else {
            s.open_palette(tx);
        }
        return true;
    }
    if e.is_command('f') && s.tab == Tab::Notebook && s.open.is_none() {
        s.palette_open = false;
        s.dismiss_menus();
        s.close_settings();
        s.notebook.pane.set_open(true);
        s.notebook.query.focused = true;
        tx.focus_when_shown("nb-search", false);
        return true;
    }
    if s.tab == Tab::Notebook && s.open.is_none() && !s.palette_open && !s.settings_open && rune_kit::shell::is_sidebar_toggle(e) {
        s.notebook.pane.toggle();
        return true;
    }
    if e.is_command('f') {
        s.palette_open = false;
        s.dismiss_menus();
        s.close_lightbox();
        s.close_settings();
        if s.open.is_some() {
            if s.files.view == crate::files::ProjectView::Files {
                s.files.view = crate::files::ProjectView::Updates;
                s.ask();
            }
            s.project_search_open = true;
            s.project_search.focused = true;
            tx.focus_when_shown("project-search", false);
            return true;
        }
        s.select_tab(Tab::Feed, tx);
        s.feed_search.focused = true;
        tx.focus_when_shown("feed-search", false);
        return true;
    }
    if s.open.is_none()
        && s.editing.is_none()
        && !nb_editing
        && !s.palette_open
        && !s.settings_open
        && let Some(tab) = ['1', '2', '3'].iter().position(|c| e.is_command(*c)).map(|i| Tab::ALL[i])
    {
        s.select_tab(tab, tx);
        return true;
    }
    if e.is_command(',') {
        s.dismiss_menus();
        s.close_lightbox();
        s.palette_open = false;
        s.delete_confirm = None;
        if s.settings_open {
            s.close_settings();
        } else {
            s.settings_open = true;
        }
        s.sync_search();
        return true;
    }
    if s.lightbox.is_some() {
        match e.key {
            Key::Escape => {
                if s.lightbox_compare.is_some() {
                    s.lightbox_compare = None;
                } else if s.lightbox_zoom.owns_escape() {
                    s.lightbox_zoom.fit(tx);
                } else {
                    s.close_lightbox();
                }
                return true;
            }
            Key::Left => {
                s.step_lightbox(-1);
                return true;
            }
            Key::Right => {
                s.step_lightbox(1);
                return true;
            }
            _ => {}
        }
    }
    if s.palette_open {
        match e.key {
            Key::Escape => {
                s.close_palette();
                return true;
            }
            Key::Up => {
                chrome::move_palette(s, -1);
                return true;
            }
            Key::Down => {
                chrome::move_palette(s, 1);
                return true;
            }
            Key::Enter => {
                chrome::run_palette(s, tx);
                return true;
            }
            _ => {}
        }
    }
    if matches!(e.key, Key::Escape) && s.delete_confirm.is_some() {
        s.delete_confirm = None;
        return true;
    }
    if matches!(e.key, Key::Escape) && s.menu_open {
        s.menu_open = false;
        return true;
    }
    if matches!(e.key, Key::Escape) && s.post_menu.is_some() {
        s.post_menu = None;
        return true;
    }
    if matches!(e.key, Key::Escape) && s.orb_menu.is_some() {
        s.orb_menu = None;
        return true;
    }
    if matches!(e.key, Key::Escape) && s.settings_open {
        if s.curator.open_select.is_some() {
            s.curator.close_select();
        } else {
            s.close_settings();
        }
        return true;
    }
    if matches!(e.key, Key::Escape) && s.curator.picker_open {
        if s.curator.open_select.is_some() {
            s.curator.close_select();
        } else {
            s.curator.close_picker();
            tx.focus_when_shown("curator-picker-chip", false);
        }
        return true;
    }
    #[cfg(feeder_sparkle)]
    if matches!(e.key, Key::Escape) && s.updates.open {
        s.updates.open = false;
        tx.focus_when_shown("feeder-update-button", false);
        return true;
    }
    if matches!(e.key, Key::Escape)
        && s.tab == Tab::Notebook
        && s.open.is_none()
        && s.notebook.editor.is_none()
        && s.notebook.open.is_none()
        && s.notebook.shelf == crate::notebook::Shelf::Gardener
        && s.notebook.chat.phase == crate::gardener::Phase::Working
    {
        s.gardener_stop();
        return true;
    }
    if matches!(e.key, Key::Escape) && s.tab == Tab::Notebook && s.open.is_none() && s.notebook_escape() {
        return true;
    }
    if matches!(e.key, Key::Escape) && s.editing_update.is_some() {
        s.editing_update = None;
        return true;
    }
    if matches!(e.key, Key::Escape) && s.replying_to.is_some() {
        s.replying_to = None;
        return true;
    }
    if matches!(e.key, Key::Escape) && s.brief_editing.is_some() {
        s.brief_editing = None;
        return true;
    }
    if matches!(e.key, Key::Escape) && s.editing.is_some() {
        s.cancel_rename();
        return true;
    }
    if matches!(e.key, Key::Escape) && s.open.is_none() && s.tab == Tab::Feed && !s.feed_search.text.is_empty() {
        s.feed_search.text.clear();
        s.sync_search();
        return true;
    }
    if matches!(e.key, Key::Escape) && s.open.is_some() && (s.project_search_open || !s.project_search.text.is_empty()) {
        if !s.project_search.text.is_empty() {
            s.project_search.text.clear();
        } else {
            s.project_search_open = false;
            s.project_search.focused = false;
        }
        return true;
    }
    if matches!(e.key, Key::Escape) && s.open.is_some() && s.files.view == crate::files::ProjectView::Files && s.files.close_preview() {
        tx.focus_when_shown("files-list", false);
        return true;
    }
    if matches!(e.key, Key::Escape) && s.open.is_some() {
        s.close_project();
        return true;
    }

    // Single-key keyboard navigation when not typing or inside a modal or menu.
    if !s.is_typing()
        && s.lightbox.is_none()
        && !s.settings_open
        && s.delete_confirm.is_none()
        && !s.menu_open
        && !s.curator.picker_open
        && s.orb_menu.is_none()
        && s.post_menu.is_none()
        && !s.notebook.action_menu_open
        && !e.mods.cmd
        && !e.mods.ctrl
        && !e.mods.alt
    {
        let key_ch = match &e.key {
            Key::Text(t) => t.chars().next(),
            _ => None,
        };
        if let Some(pid) = s.open {
            if s.files.view == crate::files::ProjectView::Updates && s.feed_for == Some(pid) {
                let ids: Vec<i64> = feed::visible_feed(s).into_iter().map(|u| u.id).collect();
                let current = s.feed_selected.filter(|id| ids.contains(id));
                match (&e.key, key_ch) {
                    (Key::Down, _) | (_, Some('j')) => {
                        s.feed_selected = step_selection(&ids, current, 1);
                        if let Some(uid) = s.feed_selected {
                            tx.scroll_into_view(format!("u-{uid}"));
                        }
                        return true;
                    }
                    (Key::Up, _) | (_, Some('k')) => {
                        s.feed_selected = step_selection(&ids, current, -1);
                        if let Some(uid) = s.feed_selected {
                            tx.scroll_into_view(format!("u-{uid}"));
                        }
                        return true;
                    }
                    (_, Some('r')) => {
                        if let Some(uid) = current.or_else(|| ids.first().copied()) {
                            s.start_reply(uid, tx);
                            return true;
                        }
                    }
                    (_, Some('e')) => {
                        if let Some(uid) = current.or_else(|| ids.first().copied()) {
                            s.start_edit_update(uid, tx);
                            return true;
                        }
                    }
                    _ => {}
                }
            }
        } else if s.tab == Tab::Home {
            let ids: Vec<i64> = home::ordered_active(s).into_iter().map(|p| p.project.id).collect();
            let current = s.home_selected.filter(|id| ids.contains(id));
            match (&e.key, key_ch) {
                (Key::Right, _) | (Key::Down, _) | (_, Some('j')) => {
                    s.home_selected = step_selection(&ids, current, 1);
                    if let Some(id) = s.home_selected {
                        tx.scroll_into_view(format!("tile-{id}"));
                    }
                    return true;
                }
                (Key::Left, _) | (Key::Up, _) | (_, Some('k')) => {
                    s.home_selected = step_selection(&ids, current, -1);
                    if let Some(id) = s.home_selected {
                        tx.scroll_into_view(format!("tile-{id}"));
                    }
                    return true;
                }
                (Key::Enter, _) => {
                    if let Some(id) = current {
                        s.open_project(id);
                        return true;
                    }
                }
                (_, Some('m')) => {
                    if let Some(id) = current {
                        s.mark_seen(id);
                        return true;
                    }
                }
                _ => {}
            }
        } else if s.tab == Tab::Feed {
            let visible = s.filtered_timeline();
            let ids: Vec<i64> = visible.iter().map(|(u, _)| u.id).collect();
            let current = s.timeline_selected.filter(|id| ids.contains(id));
            match (&e.key, key_ch) {
                (Key::Down, _) | (_, Some('j')) => {
                    s.timeline_selected = step_selection(&ids, current, 1);
                    if let Some(uid) = s.timeline_selected {
                        tx.scroll_into_view(format!("t-{uid}"));
                    }
                    return true;
                }
                (Key::Up, _) | (_, Some('k')) => {
                    s.timeline_selected = step_selection(&ids, current, -1);
                    if let Some(uid) = s.timeline_selected {
                        tx.scroll_into_view(format!("t-{uid}"));
                    }
                    return true;
                }
                (Key::Enter, _) => {
                    if let Some(uid) = current
                        && let Some((_, sum)) = visible.iter().find(|(u, _)| u.id == uid)
                    {
                        s.open_project(sum.project.id);
                        return true;
                    }
                }
                (_, Some('m')) => {
                    if let Some(uid) = current
                        && let Some((_, sum)) = visible.iter().find(|(u, _)| u.id == uid)
                    {
                        s.mark_seen(sum.project.id);
                        return true;
                    }
                }
                _ => {}
            }
        }
    }
    false
}
