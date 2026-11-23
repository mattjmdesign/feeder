//! Which subscriptions power Curator, their nicknames, and the model and thinking level each runs
//! with: Settings → Curator and the picker in Curator's composer. Signing a CLI in only makes it
//! available; Curator runs on it after it's turned on here.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};

use feeder_core::gardener::{Backend, CuratorPrefs, ModelInfo, NICKNAME_CHARS, effort_label};
use rune::{El, Key, KeyEvent, ScrollSpec, TextEdit, Tx, Waker, col, el, row, text};
use rune_kit::{
    Button, ButtonVariant, ControlSize, Kit, Placement, SegmentedControl, Select, SelectAppearance, Switch, TextInput, popover, scroll_bar,
};

use crate::style::{self, Palette};
use crate::{State, agents};

/// The CLI's default model or thinking level.
/// The CLI picks the model (its own default for that account).
const MODEL_DEFAULT: &str = "Account default";
/// The model picks its thinking level.
const DEFAULT: &str = "Default";
/// Wide enough to show full `provider/model-name` strings like `opencode-go/deepseek-v4.1-flash`.
const MODEL_FIELD_W: f32 = 320.0;
const MODEL_MENU_W: f32 = 400.0;
const PICKER_MENU_W: f32 = 380.0;
/// Show a search box inside the composer popover once a provider offers more than a handful of models.
const PICKER_SEARCH_MIN: usize = 7;

pub enum Models {
    Loading,
    Ready(Vec<ModelInfo>),
    Failed(String),
}

pub struct Curator {
    pub prefs: CuratorPrefs,
    models: HashMap<Backend, Models>,
    tx: Sender<(Backend, Result<Vec<ModelInfo>, String>)>,
    rx: Receiver<(Backend, Result<Vec<ModelInfo>, String>)>,
    nicknames: HashMap<Backend, TextEdit>,
    /// The open dropdown in Settings → Curator, by key.
    pub open_select: Option<String>,
    pub picker_open: bool,
    model_search: TextEdit,
    picker_search: TextEdit,
}

impl Default for Curator {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Curator {
            prefs: CuratorPrefs::default(),
            models: HashMap::new(),
            tx,
            rx,
            nicknames: HashMap::new(),
            open_select: None,
            picker_open: false,
            model_search: TextEdit::new(""),
            picker_search: TextEdit::new(""),
        }
    }
}

impl Curator {
    pub fn load() -> Curator {
        let prefs = CuratorPrefs::load(&feeder_core::data_dir());
        let nicknames = Backend::ALL.into_iter().map(|b| (b, TextEdit::new(prefs.get(b).nickname))).collect();
        Curator { prefs, nicknames, ..Curator::default() }
    }

    fn save(&self) {
        if let Err(e) = self.prefs.save(&feeder_core::data_dir()) {
            eprintln!("feeder: couldn't save Curator settings: {e}");
        }
    }

    pub fn edit(&mut self, f: impl FnOnce(&mut CuratorPrefs)) {
        f(&mut self.prefs);
        self.save();
    }

    /// Lists the subscription's models once, off the UI thread.
    pub fn want_models(&mut self, backend: Backend, wake: Option<&Waker>) {
        if matches!(self.models.get(&backend), Some(Models::Loading | Models::Ready(_))) {
            return;
        }
        self.models.insert(backend, Models::Loading);
        let tx = self.tx.clone();
        let wake = wake.cloned();
        let spawned = std::thread::Builder::new().name("feeder-curator-models".into()).spawn(move || {
            let result = backend.models(&feeder_core::data_dir()).map_err(|e| e.to_string());
            let _ = tx.send((backend, result));
            if let Some(wake) = wake {
                wake();
            }
        });
        if spawned.is_err() {
            self.models.insert(backend, Models::Failed("Couldn't list models.".into()));
        }
    }

    /// Sample choices for snapshot scenes: Claude Code on as "Work" with its models listed.
    /// Nothing is saved.
    pub fn demo(&mut self) {
        let mut prefs = CuratorPrefs::default();
        prefs.set_enabled(Backend::ClaudeCode, true);
        prefs.set_enabled(Backend::Codex, true);
        prefs.set_nickname(Backend::ClaudeCode, "Work");
        prefs.set_nickname(Backend::Codex, "Personal");
        prefs.edit(Backend::ClaudeCode, |p| {
            p.model = Some("sonnet".into());
            p.effort = Some("medium".into());
        });
        self.nicknames = Backend::ALL.into_iter().map(|b| (b, TextEdit::new(prefs.get(b).nickname))).collect();
        self.prefs = prefs;
        if let Ok(list) = Backend::ClaudeCode.models(&feeder_core::data_dir()) {
            self.models.insert(Backend::ClaudeCode, Models::Ready(list));
        }
        self.models.insert(Backend::Codex, Models::Loading);
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((backend, result)) = self.rx.try_recv() {
            self.models.insert(backend, result.map_or_else(Models::Failed, Models::Ready));
            changed = true;
        }
        changed
    }

    fn models(&self, backend: Backend) -> Option<&Models> {
        self.models.get(&backend)
    }

    fn model_info(&self, backend: Backend) -> Option<&ModelInfo> {
        let id = self.prefs.get(backend).model?;
        match self.models(backend)? {
            Models::Ready(list) => list.iter().find(|m| m.id == id),
            _ => None,
        }
    }

    /// The chosen model's thinking levels; empty when it has none or none is chosen.
    fn efforts(&self, backend: Backend) -> Vec<String> {
        self.model_info(backend).map(|m| m.efforts.clone()).unwrap_or_default()
    }

    fn model_label(&self, backend: Backend) -> Option<String> {
        let id = self.prefs.get(backend).model?;
        Some(self.model_info(backend).map_or(id, |m| m.label.clone()))
    }

    /// Turns on or off for Curator. Loads its models when it turns on.
    pub fn set_enabled(&mut self, backend: Backend, on: bool, wake: Option<&Waker>) {
        self.edit(|p| p.set_enabled(backend, on));
        if on {
            self.want_models(backend, wake);
        }
    }

    pub fn choose_model(&mut self, backend: Backend, id: Option<String>) {
        let efforts = id.as_ref().and_then(|id| match self.models(backend) {
            Some(Models::Ready(list)) => list.iter().find(|m| &m.id == id).map(|m| m.efforts.clone()),
            _ => None,
        });
        self.edit(|p| {
            p.edit(backend, |pr| {
                pr.model = id;
                // A level the new model doesn't offer falls back to its default.
                if pr.effort.as_ref().is_some_and(|e| !efforts.as_ref().is_some_and(|l| l.contains(e))) {
                    pr.effort = None;
                }
            });
        });
    }

    pub fn choose_effort(&mut self, backend: Backend, effort: Option<String>) {
        self.edit(|p| p.edit(backend, |pr| pr.effort = effort));
    }

    pub fn close_select(&mut self) {
        self.open_select = None;
        self.model_search = TextEdit::new("");
    }

    pub fn close_picker(&mut self) {
        self.picker_open = false;
        self.picker_search = TextEdit::new("");
        self.close_select();
    }

    pub fn is_typing(&self) -> bool {
        self.model_search.focused || self.picker_search.focused || self.nicknames.values().any(|e| e.focused)
    }
}

/// Opens Settings on the Curator page.
pub fn open_settings(s: &mut State) {
    s.settings_page = crate::chrome::SettingsPage::Curator;
    s.settings_open = true;
}

/// The provider a new chat runs on: the chosen one if it's still on and installed, else the
/// default, else the first turned on. `None` until one is turned on.
pub fn selected(s: &State) -> Option<Backend> {
    let installed = &s.notebook.gardener.backends;
    let prefs = &s.curator.prefs;
    let on = |b: &Backend| prefs.is_enabled(*b) && installed.contains(b);
    s.notebook.curator_backend.filter(on).or_else(|| prefs.default_backend().filter(on)).or_else(|| prefs.enabled().into_iter().find(on))
}

// ---------- Settings → Curator ----------

pub fn pane(s: &State, p: &Palette, k: &Kit, layer: &mut Vec<El>) -> El {
    let installed = &s.notebook.gardener.backends;
    let prefs = &s.curator.prefs;
    let mut pane = col().w_full().gap(28.0).child(header(
        p,
        "Curator",
        "Curator reads your agents’ updates and keeps the Notebook in order. It runs only on a subscription you turn on here.",
    ));

    let enabled: Vec<Backend> = prefs.enabled().into_iter().filter(|b| installed.contains(b)).collect();
    if enabled.len() > 1 {
        let names: Vec<String> = enabled.iter().map(|b| prefs.name(*b)).collect();
        let current = prefs.default_backend().filter(|b| enabled.contains(b)).or_else(|| enabled.first().copied()).map(|b| prefs.name(b));
        let choices = enabled.clone();
        let select = Select::new("curator-default", &names)
            .appearance(SelectAppearance::Field)
            .label("Default for new chats")
            .current(current.as_deref())
            .width(MODEL_FIELD_W)
            .open(s.curator.open_select.as_deref() == Some("curator-default"))
            .on_toggle(|s: &mut State| toggle_select(s, "curator-default"))
            .on_close(|s: &mut State| s.curator.close_select())
            .on_choose(move |s: &mut State, chosen: &str, _: &mut Tx| {
                let pick = choices.iter().copied().find(|b| s.curator.prefs.name(*b) == chosen);
                s.curator.edit(|p| p.default = pick.map(|b| b.as_str().to_string()));
                s.curator.close_select();
            })
            .build(k, layer);
        pane = pane.child(group(p, None, vec![labeled(p, "Default for new chats", None, select)]));
    }

    let rows: Vec<El> = if installed.is_empty() {
        vec![
            text("Install Claude Code, Codex, OpenCode, or Antigravity to use Curator.")
                .font(style::BODY)
                .color(p.text_3)
                .wrap()
                .px(16.0)
                .py(12.0),
        ]
    } else {
        installed.iter().map(|b| subscription(s, *b, p, k, layer)).collect()
    };
    pane.child(group(p, Some("Subscriptions on this Mac"), rows))
}

fn toggle_select(s: &mut State, key: &str) {
    if s.curator.open_select.as_deref() == Some(key) {
        s.curator.close_select();
    } else {
        s.curator.open_select = Some(key.to_string());
        s.curator.model_search = TextEdit::new("");
    }
}

fn subscription(s: &State, backend: Backend, p: &Palette, k: &Kit, layer: &mut Vec<El>) -> El {
    let prefs = &s.curator.prefs;
    let on = prefs.is_enabled(backend);
    let usable = s.curator_sign_in.usable(backend);
    let (status, action) = crate::curator_sign_in::cells(s, backend, p, k);
    let toggle = Switch::new(on)
        .label(if !on && !usable {
            format!("Use {} for Curator, sign in first", prefs.name(backend))
        } else {
            format!("Use {} for Curator", prefs.name(backend))
        })
        .hide_label()
        .disabled(!on && !usable)
        .on_toggle(move |s: &mut State| {
            let next = !s.curator.prefs.is_enabled(backend);
            let wake = s.wake.clone();
            s.curator.set_enabled(backend, next, wake.as_ref());
        })
        .build(k)
        .key(format!("curator-use-{}", backend.as_str()));
    let trailing = row().gap(10.0).center_y().child(action).child(toggle);
    let words =
        col().grow(1.0).min_w(0.0).gap(2.0).child(text(prefs.name(backend)).font(style::NAME).color(p.text).truncate()).child(status);
    let head = row()
        .w_full()
        .gap(12.0)
        .center_y()
        .child(el().size(20.0, 20.0).center().child(agents::mark(backend.label(), p, 18.0, s.agent_icon(backend.label()))))
        .child(words)
        .child(trailing);
    let mut card = col().key(format!("curator-sub-{}", backend.as_str())).w_full().gap(14.0).px(16.0).py(12.0).child(head);
    if on {
        card = card.child(details(s, backend, p, k, layer).pl(32.0));
    }
    card
}

/// A turned-on subscription's nickname, model, and thinking level.
fn details(s: &State, backend: Backend, p: &Palette, k: &Kit, layer: &mut Vec<El>) -> El {
    let c = &s.curator;
    let nick_edit = c.nicknames.get(&backend).cloned().unwrap_or_else(|| TextEdit::new(""));
    let nickname = TextInput::new(&nick_edit)
        .label(format!("Nickname for {}", backend.label()))
        .placeholder("Personal, Work…")
        .width(Some(MODEL_FIELD_W))
        .on_edit(move |s: &mut State| s.curator.nicknames.entry(backend).or_insert_with(|| TextEdit::new("")))
        .on_change(move |s: &mut State, _| {
            let text = s.curator.nicknames.get(&backend).map(|e| e.text.clone()).unwrap_or_default();
            if text.chars().count() > NICKNAME_CHARS {
                let cut: String = text.chars().take(NICKNAME_CHARS).collect();
                let end = cut.len();
                if let Some(edit) = s.curator.nicknames.get_mut(&backend) {
                    edit.replace_text(cut);
                    edit.set_caret(end);
                }
            }
            let saved = s.curator.nicknames.get(&backend).map(|e| e.text.clone()).unwrap_or_default();
            s.curator.edit(|p| p.set_nickname(backend, &saved));
        })
        .build(k)
        .key(format!("curator-nick-{}", backend.as_str()));

    let mut rows = col().w_full().gap(10.0).child(field(p, "Nickname", nickname));
    rows = rows.child(field(p, "Model", model_select(s, backend, p, k, layer)));
    let efforts = c.efforts(backend);
    if !efforts.is_empty() {
        rows = rows.child(field(p, "Thinking", effort_control(s, backend, &efforts, Some(MODEL_FIELD_W), k, layer)));
    }
    rows
}

fn model_matches_query(model: &ModelInfo, words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let normalized_id = model.id.replace(['/', '-', '_'], " ");
    let haystack = format!("{} {} {}", model.label, model.id, normalized_id).to_lowercase();
    words.iter().all(|w| haystack.contains(w.as_str()))
}

fn filter_models<'a>(list: &'a [ModelInfo], query: &str) -> Vec<&'a ModelInfo> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    list.iter().filter(|m| model_matches_query(m, &words)).collect()
}

fn matches_default_label(query: &str) -> bool {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return true;
    }
    let haystack = MODEL_DEFAULT.to_lowercase();
    words.iter().all(|w| haystack.contains(w.as_str()))
}

fn model_select(s: &State, backend: Backend, p: &Palette, k: &Kit, layer: &mut Vec<El>) -> El {
    let c = &s.curator;
    let key = format!("curator-model-{}", backend.as_str());
    match c.models(backend) {
        Some(Models::Ready(list)) => {
            let current = c.model_label(backend).unwrap_or_else(|| MODEL_DEFAULT.into());
            let is_open = c.open_select.as_deref() == Some(key.as_str());
            let anchor_key = format!("{key}-trigger");
            let open_key = key.clone();
            let trigger = Select::new(key.clone(), &[] as &[&str])
                .appearance(SelectAppearance::Field)
                .label("Model")
                .current(Some(&current))
                .width(MODEL_FIELD_W)
                .open(is_open)
                .trigger(k)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                    if s.curator.open_select.as_deref() == Some(open_key.as_str()) {
                        s.curator.close_select();
                    } else {
                        s.curator.open_select = Some(open_key.clone());
                        s.curator.model_search = TextEdit::new("");
                        tx.focus_when_shown("curator-model-search", false);
                    }
                });
            if !is_open {
                return trigger;
            }

            let query = c.model_search.text.trim().to_string();
            let filtered = filter_models(list, &query);
            let show_default = matches_default_label(&query);
            let chosen = c.prefs.get(backend).model;
            let first_match_id = filtered.first().map(|m| m.id.clone());
            let submit_default = show_default && (query.is_empty() || first_match_id.is_none());
            let focus_back = anchor_key.clone();
            let esc_back = anchor_key.clone();

            let mut search = TextInput::new(&c.model_search)
                .placeholder("Search models")
                .label("Search models")
                .leading_icon(k.glyph.search)
                .width(None)
                .on_edit(|s: &mut State| &mut s.curator.model_search)
                .on_action(|s: &mut State, _| s.curator.model_search = TextEdit::new(""))
                .on_submit(move |s: &mut State, tx: &mut Tx| {
                    if let Some(id) = first_match_id.clone() {
                        s.curator.choose_model(backend, Some(id));
                        s.curator.close_select();
                        tx.focus_when_shown(focus_back.clone(), false);
                    } else if submit_default {
                        s.curator.choose_model(backend, None);
                        s.curator.close_select();
                        tx.focus_when_shown(focus_back.clone(), false);
                    }
                })
                .on_key(move |s: &mut State, e: &KeyEvent, tx: &mut Tx| {
                    if matches!(e.key, Key::Escape) {
                        if !s.curator.model_search.text.is_empty() {
                            s.curator.model_search = TextEdit::new("");
                        } else {
                            s.curator.close_select();
                            tx.focus_when_shown(esc_back.clone(), false);
                        }
                        return true;
                    }
                    false
                });
            if !c.model_search.text.is_empty() {
                search = search.clear_button();
            }

            let mut rows = col().w_full().gap(2.0).instant_changes();
            if show_default {
                let back = anchor_key.clone();
                rows =
                    rows.child(option_row_tx(p, MODEL_DEFAULT.to_string(), chosen.is_none(), None, move |s: &mut State, tx: &mut Tx| {
                        s.curator.choose_model(backend, None);
                        s.curator.close_select();
                        tx.focus_when_shown(back.clone(), false);
                    }));
            }
            for m in &filtered {
                let id = m.id.clone();
                let back = anchor_key.clone();
                rows = rows.child(option_row_tx(
                    p,
                    m.label.clone(),
                    chosen.as_deref() == Some(m.id.as_str()),
                    None,
                    move |s: &mut State, tx: &mut Tx| {
                        s.curator.choose_model(backend, Some(id.clone()));
                        s.curator.close_select();
                        tx.focus_when_shown(back.clone(), false);
                    },
                ));
            }
            if !show_default && filtered.is_empty() {
                rows = rows.child(text(format!("No models match “{query}”")).font(style::META).color(p.text_3).wrap().px(8.0).py(10.0));
            }

            let scroll_viewport = col().w_full().max_h(280.0).shrink().min_h(0.0).child(rows).scroll(ScrollSpec { stick_bottom: false });
            let list_el =
                scroll_bar(k, &format!("{key}-scroll-area"), &format!("{key}-scroll"), scroll_viewport).w_full().shrink().min_h(0.0);

            let panel = col()
                .key(format!("{key}-content"))
                .role("dialog")
                .label("Model")
                .focus_return(&anchor_key)
                .w(MODEL_MENU_W)
                .shrink()
                .min_h(0.0)
                .padding(6.0)
                .gap(6.0)
                .child(search.build(k).key("curator-model-search"))
                .child(el().w_full().h(1.0).fill(p.hairline))
                .child(list_el);

            layer.push(
                el().key(format!("{key}-scrim"))
                    .role("none")
                    .absolute(0.0, 0.0)
                    .w_full()
                    .h_full()
                    .on_press(|s: &mut State| s.curator.close_select()),
            );
            layer.push(popover::anchored(k, popover::raised(k, panel, k.radius.md), &anchor_key, Placement::BottomEnd, 4.0));
            trigger
        }
        Some(Models::Failed(e)) => {
            row().gap(8.0).center_y().child(text("Couldn’t list models").font(style::META).color(k.c.danger.fg).help(e.clone())).child(
                Button::new("Retry")
                    .variant(ButtonVariant::GhostMuted)
                    .control_size(ControlSize::Small)
                    .on_press(move |s: &mut State| {
                        s.curator.models.remove(&backend);
                        let wake = s.wake.clone();
                        s.curator.want_models(backend, wake.as_ref());
                    })
                    .build(k),
            )
        }
        Some(Models::Loading) | None => text("Loading models…").font(style::META).color(k.c.std_text),
    }
}

/// Up to four levels fit a segmented control; more (Codex lists six) use a dropdown.
fn effort_control(s: &State, backend: Backend, efforts: &[String], field_w: Option<f32>, k: &Kit, layer: &mut Vec<El>) -> El {
    let current = s.curator.prefs.get(backend).effort;
    let mut labels = vec![DEFAULT.to_string()];
    labels.extend(efforts.iter().map(|e| effort_label(e)));
    let values: Vec<Option<String>> = std::iter::once(None).chain(efforts.iter().cloned().map(Some)).collect();
    let selected = values.iter().position(|v| *v == current).unwrap_or(0);
    if labels.len() <= 4 {
        let mut seg = SegmentedControl::new(&labels, Some(selected))
            .label("Thinking")
            .on_pick(move |s: &mut State, i: usize, _: &mut Tx| s.curator.choose_effort(backend, values.get(i).cloned().flatten()));
        if field_w.is_none() {
            seg = seg.fit();
        }
        return seg.build(k);
    }
    let key = format!("curator-effort-{}", backend.as_str());
    let open_key = key.clone();
    let names = labels.clone();
    let mut sel = Select::new(key.clone(), &labels)
        .label("Thinking")
        .current(Some(&labels[selected]))
        .open(s.curator.open_select.as_deref() == Some(key.as_str()))
        .on_toggle(move |s: &mut State| toggle_select(s, &open_key))
        .on_close(|s: &mut State| s.curator.close_select())
        .on_choose(move |s: &mut State, chosen: &str, _: &mut Tx| {
            let i = names.iter().position(|l| l == chosen).unwrap_or(0);
            s.curator.choose_effort(backend, values.get(i).cloned().flatten());
            s.curator.close_select();
        });
    if let Some(w) = field_w {
        sel = sel.appearance(SelectAppearance::Field).width(w);
    }
    sel.build(k, layer)
}

// ---------- Composer picker ----------

/// The chip beside Send: the subscription's mark, then its model and thinking level. Opens a
/// popover to switch subscription, model, and thinking. Shows the running chat's choice mid-chat.
pub fn picker(s: &State, current: Backend, locked: bool, p: &Palette, k: &Kit, layer: &mut Vec<El>) -> El {
    let c = &s.curator;
    let prefs = &c.prefs;
    // Which subscription first, so work and personal read apart at a glance.
    let mut summary = prefs.name(current);
    if let Some(model) = c.model_label(current) {
        summary = format!("{summary} · {model}");
    }
    if let Some(effort) = prefs.get(current).effort {
        summary = format!("{summary} · {}", effort_label(&effort));
    }
    let chip = row()
        .key("curator-picker-chip")
        .gap(6.0)
        .center_y()
        .px(8.0)
        .h(26.0)
        .radius(7.0)
        .role("button")
        .expanded(c.picker_open)
        .label(format!("Curator runs on {summary}"))
        .child(agents::mark(current.label(), p, 14.0, s.agent_icon(current.label())))
        .child(text(summary).font(style::AGENT).color(p.text).truncate().max_w(320.0));
    let chip = if locked {
        chip.help("Start a new chat to switch")
    } else {
        chip.focusable(true).focus_ring(2.0, 1.0, k.c.border_focus).hover(p.hover).on_press_tx(|s: &mut State, tx: &mut Tx| {
            toggle_picker(s, tx);
        })
    };
    if locked {
        return chip;
    }

    let anchor_key = "curator-picker-trigger";
    let trigger = row().key(anchor_key).child(chip);
    if !c.picker_open {
        return trigger;
    }

    let installed = &s.notebook.gardener.backends;
    let mut body = col().w(PICKER_MENU_W).shrink().min_h(0.0).padding(6.0).gap(4.0);
    let subs: Vec<Backend> = prefs.enabled().into_iter().filter(|b| installed.contains(b)).collect();
    if subs.len() > 1 {
        let mut list = col().w_full().gap(2.0);
        for b in subs {
            list = list.child(option_row(
                p,
                prefs.name(b),
                b == current,
                Some(agents::mark(b.label(), p, 14.0, s.agent_icon(b.label()))),
                move |s: &mut State| {
                    s.notebook.curator_backend = Some(b);
                    s.curator.picker_search = TextEdit::new("");
                    let wake = s.wake.clone();
                    s.curator.want_models(b, wake.as_ref());
                },
            ));
        }
        body = body.child(list).child(divider(p));
    }

    match c.models(current) {
        Some(Models::Ready(list)) => {
            let searchable = list.len() >= PICKER_SEARCH_MIN;
            let query = if searchable { c.picker_search.text.trim().to_string() } else { String::new() };
            let filtered = filter_models(list, &query);
            let show_default = matches_default_label(&query);
            let chosen = prefs.get(current).model;

            if searchable {
                let first_match_id = filtered.first().map(|m| m.id.clone());
                let submit_default = show_default && (query.is_empty() || first_match_id.is_none());
                let mut search = TextInput::new(&c.picker_search)
                    .placeholder("Search models")
                    .label("Search models")
                    .leading_icon(k.glyph.search)
                    .width(None)
                    .on_edit(|s: &mut State| &mut s.curator.picker_search)
                    .on_action(|s: &mut State, _| s.curator.picker_search = TextEdit::new(""))
                    .on_submit(move |s: &mut State, tx: &mut Tx| {
                        if let Some(id) = first_match_id.clone() {
                            s.curator.choose_model(current, Some(id));
                            s.curator.close_picker();
                            tx.focus_when_shown("curator-picker-chip", false);
                        } else if submit_default {
                            s.curator.choose_model(current, None);
                            s.curator.close_picker();
                            tx.focus_when_shown("curator-picker-chip", false);
                        }
                    })
                    .on_key(|s: &mut State, e: &KeyEvent, tx: &mut Tx| {
                        if matches!(e.key, Key::Escape) {
                            if !s.curator.picker_search.text.is_empty() {
                                s.curator.picker_search = TextEdit::new("");
                            } else {
                                s.curator.close_picker();
                                tx.focus_when_shown("curator-picker-chip", false);
                            }
                            return true;
                        }
                        false
                    });
                if !c.picker_search.text.is_empty() {
                    search = search.clear_button();
                }
                body = body.child(search.build(k).key("curator-picker-search"));
            }

            let mut models = col().w_full().gap(2.0).instant_changes();
            if show_default {
                models = models.child(option_row(p, MODEL_DEFAULT.to_string(), chosen.is_none(), None, move |s: &mut State| {
                    s.curator.choose_model(current, None);
                }));
            }
            for m in &filtered {
                let id = m.id.clone();
                models =
                    models.child(option_row(p, m.label.clone(), chosen.as_deref() == Some(m.id.as_str()), None, move |s: &mut State| {
                        s.curator.choose_model(current, Some(id.clone()));
                    }));
            }
            if !show_default && filtered.is_empty() {
                models = models.child(text(format!("No models match “{query}”")).font(style::META).color(p.text_3).wrap().px(8.0).py(10.0));
            }
            let scroll_viewport = col().w_full().max_h(240.0).shrink().min_h(0.0).child(models).scroll(ScrollSpec { stick_bottom: false });
            body = body
                .child(scroll_bar(k, "curator-picker-models-area", "curator-picker-models", scroll_viewport).w_full().shrink().min_h(0.0));
        }
        Some(Models::Failed(e)) => body = body.child(text(e.clone()).font(style::META).color(k.c.danger.fg).wrap().px(8.0).py(6.0)),
        Some(Models::Loading) | None => body = body.child(text("Loading models…").font(style::META).color(p.text_3).px(8.0).py(6.0)),
    }

    let mut sub_layer = Vec::new();
    let efforts = c.efforts(current);
    if !efforts.is_empty() {
        body = body.child(divider(p)).child(
            row()
                .w_full()
                .gap(12.0)
                .center_y()
                .px(8.0)
                .py(4.0)
                .child(text("Thinking").font(style::CONTROL).color(p.text_2).grow(1.0))
                .child(effort_control(s, current, &efforts, None, k, &mut sub_layer)),
        );
    }
    body = body.child(divider(p)).child(
        Button::new("Curator Settings…")
            .variant(ButtonVariant::GhostMuted)
            .control_size(ControlSize::Small)
            .on_press(|s: &mut State| {
                s.curator.close_picker();
                open_settings(s);
            })
            .build(k)
            .self_start(),
    );

    let panel =
        col().key("curator-picker-content").role("dialog").label("Curator model").focus_return(anchor_key).shrink().min_h(0.0).child(body);
    layer.push(
        el().key("curator-picker-scrim")
            .role("none")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .on_press(|s: &mut State| s.curator.close_picker()),
    );
    layer.push(popover::anchored(k, popover::raised(k, panel, k.radius.md), anchor_key, Placement::TopEnd, 4.0));
    layer.extend(sub_layer);
    trigger
}

fn toggle_picker(s: &mut State, tx: &mut Tx) {
    if s.curator.picker_open {
        s.curator.close_picker();
        return;
    }
    s.curator.picker_open = true;
    s.curator.picker_search = TextEdit::new("");
    if let Some(b) = selected(s) {
        let wake = s.wake.clone();
        s.curator.want_models(b, wake.as_ref());
        if matches!(s.curator.models(b), Some(Models::Ready(list)) if list.len() >= PICKER_SEARCH_MIN) {
            tx.focus_when_shown("curator-picker-search", false);
        }
    }
}

fn option_row(p: &Palette, label: String, checked: bool, mark: Option<El>, press: impl Fn(&mut State) + 'static) -> El {
    option_row_base(p, label, checked, mark).on_press(press)
}

fn option_row_tx(p: &Palette, label: String, checked: bool, mark: Option<El>, press: impl Fn(&mut State, &mut Tx) + 'static) -> El {
    option_row_base(p, label, checked, mark).on_press_tx(press)
}

fn option_row_base(p: &Palette, label: String, checked: bool, mark: Option<El>) -> El {
    let check = el().size(14.0, 14.0).center().children(checked.then(|| text("✓").font(style::CONTROL).color(p.text)));
    let mut r = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .px(8.0)
        .h(30.0)
        .radius(6.0)
        .role("menuitemradio")
        .checked(checked)
        .focusable(true)
        .hover(p.hover)
        .child(check);
    if let Some(m) = mark {
        r = r.child(m);
    }
    r.child(text(label.clone()).font(if checked { style::CONTROL } else { style::BODY }).color(p.text).truncate().grow(1.0)).label(label)
}

fn divider(p: &Palette) -> El {
    el().w_full().h(1.0).fill(p.hairline).my(4.0)
}

// ---------- Shared Settings pieces ----------

/// A page title, with one line under it only when the page needs it explained.
pub fn header(p: &Palette, title: &str, lede: &str) -> El {
    let mut c = col().w_full().gap(6.0).child(text(title.to_string()).font(style::DISPLAY).color(p.text));
    if !lede.is_empty() {
        c = c.child(text(lede.to_string()).font(style::BODY).color(p.text_2).wrap().max_w(520.0));
    }
    c
}

/// A titled, bordered list; rows are separated by hairlines.
pub fn group(p: &Palette, title: Option<&str>, rows: Vec<El>) -> El {
    let mut list = col().w_full().radius(12.0).border(1.0, p.hairline).clip_rounded(12.0);
    let count = rows.len();
    for (i, r) in rows.into_iter().enumerate() {
        list = list.child(r);
        if i + 1 < count {
            list = list.child(el().w_full().h(1.0).fill(p.hairline));
        }
    }
    let mut out = col().w_full().gap(10.0);
    if let Some(t) = title {
        out = out.child(text(t.to_string()).font(style::NAME).color(p.text_2));
    }
    out.child(list)
}

pub fn labeled(p: &Palette, label: &str, hint: Option<&str>, control: El) -> El {
    let mut words = col().grow(1.0).min_w(0.0).gap(2.0).child(text(label.to_string()).font(style::NAME).color(p.text).truncate());
    if let Some(h) = hint {
        words = words.child(text(h.to_string()).font(style::META).color(p.text_3).wrap());
    }
    row().w_full().gap(16.0).center_y().px(16.0).py(10.0).child(words).child(control)
}

/// A label and control without row padding, for the settings inside a subscription.
fn field(p: &Palette, label: &str, control: El) -> El {
    row().w_full().gap(16.0).center_y().child(text(label.to_string()).font(style::BODY).color(p.text_2).grow(1.0)).child(control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_search_matches_spaced_provider_and_version_terms() {
        let models = vec![
            ModelInfo { id: "openai/gpt-5.6-luna".into(), label: "openai/gpt-5.6-luna".into(), efforts: vec![] },
            ModelInfo { id: "opencode-go/deepseek-v4-pro".into(), label: "opencode-go/deepseek-v4-pro".into(), efforts: vec![] },
            ModelInfo { id: "opencode-go/deepseek-v4.1-flash".into(), label: "opencode-go/deepseek-v4.1-flash".into(), efforts: vec![] },
        ];
        let hits = filter_models(&models, "opencode-go deepseek v4.1 flash");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "opencode-go/deepseek-v4.1-flash");

        let hits = filter_models(&models, "opencode go 4.1");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "opencode-go/deepseek-v4.1-flash");

        let hits = filter_models(&models, "deepseek");
        assert_eq!(hits.len(), 2);

        assert!(matches_default_label(""));
        assert!(matches_default_label("default"));
        assert!(!matches_default_label("deepseek"));
    }
}
