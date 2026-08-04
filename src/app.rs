use std::cmp::Reverse;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use serde_json::{Value, json};

use crate::config::{
    ConfigDocument, ConfigError, Diagnostic, ModelSummary, ProviderSummary, Severity,
};
use crate::discovery::{DiscoveredModel, DiscoveryRequest, discover_models};
use crate::editor::{
    FormAction, FormState, FormSubmission, PROVIDER_TEMPLATES, ProviderTemplateInfo,
    provider_template,
};
use crate::known_models::{KnownModel, fetch_known_models};

const HISTORY_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Providers,
    Models,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub kind: StatusKind,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct VisibleProvider {
    pub source_index: usize,
    pub summary: ProviderSummary,
}

#[derive(Debug, Clone)]
pub struct VisibleModel {
    pub source_index: usize,
    pub summary: ModelSummary,
}

#[derive(Debug, Clone)]
pub struct DiscoveryChoice {
    pub model: DiscoveredModel,
    pub selected: bool,
    pub exists: bool,
}

#[derive(Debug, Clone)]
pub struct KnownModelChoice {
    pub model: KnownModel,
    pub exists: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogFocus {
    Content,
    Actions(usize),
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteProvider {
        provider_id: String,
    },
    DeleteModel {
        provider_id: String,
        model_index: usize,
        model_id: String,
    },
    Quit,
    Reload,
    ForceSave,
}

#[derive(Debug, Clone)]
pub enum Overlay {
    Help {
        scroll: usize,
    },
    Diagnostics {
        scroll: usize,
    },
    Templates {
        selected: usize,
        focus: DialogFocus,
    },
    Confirm {
        title: String,
        message: String,
        action: ConfirmAction,
        selected_button: usize,
    },
    Form(FormState),
    DiscoveryLoading {
        provider_id: String,
    },
    DiscoveryPicker {
        provider_id: String,
        choices: Vec<DiscoveryChoice>,
        filtered: Vec<usize>,
        query: String,
        filter_active: bool,
        cursor: usize,
        scroll: usize,
        focus: DialogFocus,
    },
    KnownModelsLoading {
        provider_id: String,
    },
    KnownModelsPicker {
        provider_id: String,
        choices: Vec<KnownModelChoice>,
        filtered: Vec<usize>,
        query: String,
        cursor: usize,
        scroll: usize,
        focus: DialogFocus,
    },
}

struct DiscoveryResult {
    provider_id: String,
    result: Result<Vec<DiscoveredModel>, String>,
}

struct KnownModelsResult {
    provider_id: String,
    result: Result<Vec<KnownModel>, String>,
}

pub struct App {
    pub doc: ConfigDocument,
    pub focus: Pane,
    pub provider_cursor: usize,
    pub model_cursor: usize,
    pub search: String,
    pub search_active: bool,
    pub overlay: Option<Overlay>,
    pub status: StatusMessage,
    pub should_quit: bool,
    pub read_only: bool,
    pub create_backup: bool,
    saved_root: Value,
    undo: Vec<Value>,
    redo: Vec<Value>,
    discovery_rx: Option<Receiver<DiscoveryResult>>,
    known_models_rx: Option<Receiver<KnownModelsResult>>,
    last_list_click: Option<(Pane, usize, Instant)>,
}

impl App {
    pub fn new(doc: ConfigDocument, read_only: bool, create_backup: bool) -> Self {
        let saved_root = doc.root().clone();
        let status = if read_only {
            StatusMessage {
                kind: StatusKind::Warning,
                text: "只读模式".into(),
            }
        } else if doc.file_exists() {
            StatusMessage {
                kind: StatusKind::Info,
                text: "配置已载入".into(),
            }
        } else {
            StatusMessage {
                kind: StatusKind::Info,
                text: "文件不存在；首次保存时创建".into(),
            }
        };
        let mut app = Self {
            doc,
            focus: Pane::Providers,
            provider_cursor: 0,
            model_cursor: 0,
            search: String::new(),
            search_active: false,
            overlay: None,
            status,
            should_quit: false,
            read_only,
            create_backup,
            saved_root,
            undo: Vec::new(),
            redo: Vec::new(),
            discovery_rx: None,
            known_models_rx: None,
            last_list_click: None,
        };
        app.normalize_selection();
        app
    }

    pub fn is_dirty(&self) -> bool {
        self.doc.root() != &self.saved_root
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.doc.validate()
    }

    pub fn diagnostic_counts(&self) -> (usize, usize) {
        self.diagnostics()
            .iter()
            .fold((0, 0), |(errors, warnings), item| match item.severity {
                Severity::Error => (errors + 1, warnings),
                Severity::Warning => (errors, warnings + 1),
            })
    }

    pub fn visible_providers(&self) -> Vec<VisibleProvider> {
        let providers = self.doc.providers();
        if self.search.trim().is_empty() {
            return providers
                .into_iter()
                .enumerate()
                .map(|(source_index, summary)| VisibleProvider {
                    source_index,
                    summary,
                })
                .collect();
        }

        let matcher = SkimMatcherV2::default().ignore_case();
        let query = self.search.trim();
        let mut matched = providers
            .into_iter()
            .enumerate()
            .filter_map(|(source_index, summary)| {
                let own_score = provider_search_text(&summary)
                    .iter()
                    .filter_map(|text| matcher.fuzzy_match(text, query))
                    .max();
                let model_score = self
                    .doc
                    .models(&summary.id)
                    .iter()
                    .flat_map(model_search_text)
                    .filter_map(|text| matcher.fuzzy_match(&text, query))
                    .max();
                own_score.max(model_score).map(|score| {
                    (
                        Reverse(score),
                        VisibleProvider {
                            source_index,
                            summary,
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        matched.sort_by_key(|(score, item)| (*score, item.source_index));
        matched.into_iter().map(|(_, item)| item).collect()
    }

    pub fn visible_models(&self) -> Vec<VisibleModel> {
        let Some(provider) = self.selected_provider() else {
            return Vec::new();
        };
        let models = self.doc.models(&provider.summary.id);
        if self.search.trim().is_empty() || self.provider_matches_query(&provider.summary) {
            return models
                .into_iter()
                .enumerate()
                .map(|(source_index, summary)| VisibleModel {
                    source_index,
                    summary,
                })
                .collect();
        }

        let matcher = SkimMatcherV2::default().ignore_case();
        let query = self.search.trim();
        let mut matched = models
            .into_iter()
            .enumerate()
            .filter_map(|(source_index, summary)| {
                model_search_text(&summary)
                    .iter()
                    .filter_map(|text| matcher.fuzzy_match(text, query))
                    .max()
                    .map(|score| {
                        (
                            Reverse(score),
                            VisibleModel {
                                source_index,
                                summary,
                            },
                        )
                    })
            })
            .collect::<Vec<_>>();
        matched.sort_by_key(|(score, item)| (*score, item.source_index));
        matched.into_iter().map(|(_, item)| item).collect()
    }

    pub fn selected_provider(&self) -> Option<VisibleProvider> {
        self.visible_providers().get(self.provider_cursor).cloned()
    }

    pub fn selected_model(&self) -> Option<VisibleModel> {
        self.visible_models().get(self.model_cursor).cloned()
    }

    pub fn poll_background(&mut self) {
        self.poll_known_models();
        let result = match self.discovery_rx.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => {
                self.discovery_rx = None;
                if matches!(self.overlay, Some(Overlay::DiscoveryLoading { .. })) {
                    self.overlay = None;
                    self.set_status(StatusKind::Error, "模型发现任务意外结束");
                }
                None
            }
            Some(Err(TryRecvError::Empty)) | None => None,
        };
        let Some(result) = result else {
            return;
        };
        self.discovery_rx = None;
        match result.result {
            Ok(models) => {
                let existing: std::collections::HashSet<_> = self
                    .doc
                    .models(&result.provider_id)
                    .into_iter()
                    .map(|model| model.id)
                    .collect();
                let choices = models
                    .into_iter()
                    .map(|model| {
                        let exists = existing.contains(&model.id);
                        DiscoveryChoice {
                            model,
                            selected: !exists,
                            exists,
                        }
                    })
                    .collect::<Vec<_>>();
                let count = choices.len();
                self.overlay = Some(Overlay::DiscoveryPicker {
                    provider_id: result.provider_id,
                    choices,
                    filtered: (0..count).collect(),
                    query: String::new(),
                    filter_active: false,
                    cursor: 0,
                    scroll: 0,
                    focus: DialogFocus::Content,
                });
                self.set_status(StatusKind::Success, format!("发现 {count} 个模型"));
            }
            Err(error) => {
                self.overlay = None;
                self.set_status(StatusKind::Error, error);
            }
        }
    }

    pub fn mouse_select_list(&mut self, pane: Pane, index: usize, activate: bool) {
        let valid = match pane {
            Pane::Providers => index < self.visible_providers().len(),
            Pane::Models => index < self.visible_models().len(),
        };
        if !valid {
            return;
        }

        let now = Instant::now();
        let double_click = self
            .last_list_click
            .is_some_and(|(last_pane, last_index, at)| {
                last_pane == pane
                    && last_index == index
                    && now.duration_since(at) <= Duration::from_millis(450)
            });
        match pane {
            Pane::Providers => {
                if self.provider_cursor != index {
                    self.model_cursor = 0;
                }
                self.provider_cursor = index;
            }
            Pane::Models => self.model_cursor = index,
        }
        self.focus = pane;

        if activate || double_click {
            self.last_list_click = None;
            self.edit_selected();
        } else {
            self.last_list_click = Some((pane, index, now));
        }
    }

    pub fn mouse_focus_pane(&mut self, pane: Pane) {
        self.focus = pane;
        self.last_list_click = None;
    }

    pub fn mouse_scroll_list(&mut self, pane: Pane, amount: isize) {
        self.focus = pane;
        self.last_list_click = None;
        self.move_selection(amount);
    }

    pub fn mouse_activate_search(&mut self) {
        self.search_active = true;
        self.last_list_click = None;
    }

    pub fn mouse_clear_search(&mut self) {
        self.search.clear();
        self.search_active = false;
        self.provider_cursor = 0;
        self.model_cursor = 0;
        self.last_list_click = None;
        self.normalize_selection();
    }

    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                self.last_list_click = None;
                if self.overlay.is_some() {
                    self.handle_overlay_key(key);
                } else if self.search_active {
                    self.handle_search_key(key);
                } else {
                    self.handle_normal_key(key);
                }
            }
            Event::Paste(text) => {
                if let Some(Overlay::Form(form)) = self.overlay.as_mut() {
                    form.insert_paste(&text);
                } else if let Some(Overlay::DiscoveryPicker {
                    choices,
                    filtered,
                    query,
                    filter_active: true,
                    cursor,
                    scroll,
                    ..
                }) = self.overlay.as_mut()
                {
                    query.push_str(&text.replace(['\r', '\n'], " "));
                    *filtered = filter_discovered_models(choices, query);
                    *cursor = 0;
                    *scroll = 0;
                } else if self.search_active {
                    self.search.push_str(&text.replace(['\r', '\n'], " "));
                    self.normalize_selection();
                }
            }
            _ => {}
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => self.save(false),
                KeyCode::Char('z') => self.undo(),
                KeyCode::Char('y') => self.redo(),
                KeyCode::Char('c') => self.request_quit(),
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.overlay = Some(Overlay::Help { scroll: 0 });
            }
            KeyCode::Char('/') => self.search_active = true,
            KeyCode::Esc if !self.search.is_empty() => {
                self.search.clear();
                self.normalize_selection();
            }
            KeyCode::Tab => self.focus_next(),
            KeyCode::BackTab => self.focus_previous(),
            KeyCode::Left => {
                self.focus = Pane::Providers;
            }
            KeyCode::Right => {
                if self.selected_provider().is_some() {
                    self.focus = Pane::Models;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Home | KeyCode::Char('g') => self.move_to_edge(false),
            KeyCode::End | KeyCode::Char('G') => self.move_to_edge(true),
            KeyCode::Enter | KeyCode::Char('e') => self.edit_selected(),
            KeyCode::Char('n') => match self.focus {
                Pane::Providers => self.open_templates(),
                Pane::Models => self.new_model(),
            },
            KeyCode::Char('p') => self.open_templates(),
            KeyCode::Char('m') => self.new_model(),
            KeyCode::Char('i') if self.focus == Pane::Models => self.start_known_models(),
            KeyCode::Char('d') | KeyCode::Delete => self.confirm_delete(),
            KeyCode::Char('c') => self.duplicate_selected(),
            KeyCode::Char('f') => self.start_discovery(),
            KeyCode::Char('v') => self.overlay = Some(Overlay::Diagnostics { scroll: 0 }),
            KeyCode::Char('r') => self.request_reload(),
            KeyCode::Char('s') => self.save(false),
            _ => {}
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.search_active = false,
            KeyCode::Backspace => {
                self.search.pop();
                self.normalize_selection();
            }
            KeyCode::Delete if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.clear();
                self.normalize_selection();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.clear();
                self.normalize_selection();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.search.push(character);
                self.normalize_selection();
            }
            _ => {}
        }
    }

    fn handle_overlay_key(&mut self, key: KeyEvent) {
        let Some(overlay) = self.overlay.take() else {
            return;
        };
        match overlay {
            Overlay::Help { mut scroll } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(1) => {}
                KeyCode::Up | KeyCode::Char('k') => {
                    scroll = scroll.saturating_sub(1);
                    self.overlay = Some(Overlay::Help { scroll });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    scroll = scroll.saturating_add(1);
                    self.overlay = Some(Overlay::Help { scroll });
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(10);
                    self.overlay = Some(Overlay::Help { scroll });
                }
                KeyCode::PageDown => {
                    scroll = scroll.saturating_add(10);
                    self.overlay = Some(Overlay::Help { scroll });
                }
                _ => self.overlay = Some(Overlay::Help { scroll }),
            },
            Overlay::Diagnostics { mut scroll } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('v') => {}
                KeyCode::Up | KeyCode::Char('k') => {
                    scroll = scroll.saturating_sub(1);
                    self.overlay = Some(Overlay::Diagnostics { scroll });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    scroll = scroll.saturating_add(1);
                    self.overlay = Some(Overlay::Diagnostics { scroll });
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(10);
                    self.overlay = Some(Overlay::Diagnostics { scroll });
                }
                KeyCode::PageDown => {
                    scroll = scroll.saturating_add(10);
                    self.overlay = Some(Overlay::Diagnostics { scroll });
                }
                _ => self.overlay = Some(Overlay::Diagnostics { scroll }),
            },
            Overlay::Templates {
                mut selected,
                mut focus,
            } => {
                let mut keep_open = true;
                match focus {
                    DialogFocus::Content => match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => keep_open = false,
                        KeyCode::Enter => {
                            self.new_provider_from_template(PROVIDER_TEMPLATES[selected]);
                            return;
                        }
                        KeyCode::Tab => focus = DialogFocus::Actions(0),
                        KeyCode::BackTab => focus = DialogFocus::Actions(1),
                        KeyCode::Up | KeyCode::Char('k') => {
                            selected = selected.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if selected + 1 < PROVIDER_TEMPLATES.len() {
                                selected += 1;
                            } else {
                                focus = DialogFocus::Actions(0);
                            }
                        }
                        KeyCode::PageUp => selected = selected.saturating_sub(5),
                        KeyCode::PageDown => {
                            selected = (selected + 5).min(PROVIDER_TEMPLATES.len() - 1);
                        }
                        KeyCode::Home | KeyCode::Char('g') => selected = 0,
                        KeyCode::End | KeyCode::Char('G') => {
                            selected = PROVIDER_TEMPLATES.len() - 1;
                        }
                        _ => {}
                    },
                    DialogFocus::Actions(mut button) => match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => keep_open = false,
                        KeyCode::Tab | KeyCode::BackTab | KeyCode::Up => {
                            focus = DialogFocus::Content;
                        }
                        KeyCode::Left => {
                            button = button.saturating_sub(1);
                            focus = DialogFocus::Actions(button);
                        }
                        KeyCode::Right => {
                            button = (button + 1).min(1);
                            focus = DialogFocus::Actions(button);
                        }
                        KeyCode::Home => focus = DialogFocus::Actions(0),
                        KeyCode::End => focus = DialogFocus::Actions(1),
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            if button == 0 {
                                self.new_provider_from_template(PROVIDER_TEMPLATES[selected]);
                                return;
                            }
                            keep_open = false;
                        }
                        _ => {}
                    },
                }
                if keep_open {
                    self.overlay = Some(Overlay::Templates { selected, focus });
                }
            }
            Overlay::Confirm {
                title,
                message,
                action,
                mut selected_button,
            } => {
                let mut keep_open = true;
                match key.code {
                    KeyCode::Char('y') => {
                        self.execute_confirm(action);
                        return;
                    }
                    KeyCode::Char('n') | KeyCode::Esc => keep_open = false,
                    KeyCode::Left | KeyCode::BackTab => {
                        selected_button = selected_button.saturating_sub(1);
                    }
                    KeyCode::Right | KeyCode::Tab => {
                        selected_button = (selected_button + 1).min(1);
                    }
                    KeyCode::Home => selected_button = 0,
                    KeyCode::End => selected_button = 1,
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if selected_button == 0 {
                            self.execute_confirm(action);
                            return;
                        }
                        keep_open = false;
                    }
                    _ => {}
                }
                if keep_open {
                    self.overlay = Some(Overlay::Confirm {
                        title,
                        message,
                        action,
                        selected_button,
                    });
                }
            }
            Overlay::Form(mut form) => match form.handle_key(key) {
                FormAction::None => self.overlay = Some(Overlay::Form(form)),
                FormAction::Cancel => {}
                FormAction::Submit => {
                    if let Err(error) = self.apply_form(&form) {
                        form.error = Some(error);
                        self.overlay = Some(Overlay::Form(form));
                    }
                }
            },
            Overlay::DiscoveryLoading { provider_id } => {
                if key.code == KeyCode::Esc {
                    self.discovery_rx = None;
                    self.set_status(StatusKind::Warning, "已忽略模型发现结果");
                } else {
                    self.overlay = Some(Overlay::DiscoveryLoading { provider_id });
                }
            }
            Overlay::DiscoveryPicker {
                provider_id,
                mut choices,
                mut filtered,
                mut query,
                mut filter_active,
                mut cursor,
                mut scroll,
                mut focus,
            } => {
                let mut keep_open = true;
                if filter_active {
                    match key.code {
                        KeyCode::Esc | KeyCode::Enter => filter_active = false,
                        KeyCode::Backspace => {
                            query.pop();
                            filtered = filter_discovered_models(&choices, &query);
                            cursor = 0;
                            scroll = 0;
                        }
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            query.clear();
                            filtered = filter_discovered_models(&choices, &query);
                            cursor = 0;
                            scroll = 0;
                        }
                        KeyCode::Char(character)
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            query.push(character);
                            filtered = filter_discovered_models(&choices, &query);
                            cursor = 0;
                            scroll = 0;
                        }
                        _ => {}
                    }
                } else {
                    match focus {
                        DialogFocus::Content => match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => keep_open = false,
                            KeyCode::Enter => {
                                self.import_discovered(&provider_id, &choices);
                                return;
                            }
                            KeyCode::Char('/') => filter_active = true,
                            KeyCode::Tab => focus = DialogFocus::Actions(0),
                            KeyCode::BackTab => focus = DialogFocus::Actions(3),
                            KeyCode::Up | KeyCode::Char('k') => {
                                cursor = cursor.saturating_sub(1);
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                if cursor + 1 < filtered.len() {
                                    cursor += 1;
                                } else {
                                    focus = DialogFocus::Actions(0);
                                }
                            }
                            KeyCode::PageUp => cursor = cursor.saturating_sub(10),
                            KeyCode::PageDown => {
                                cursor = (cursor + 10).min(filtered.len().saturating_sub(1));
                            }
                            KeyCode::Home | KeyCode::Char('g') => cursor = 0,
                            KeyCode::End | KeyCode::Char('G') => {
                                cursor = filtered.len().saturating_sub(1);
                            }
                            KeyCode::Char(' ') => {
                                if let Some(choice) = filtered
                                    .get(cursor)
                                    .and_then(|index| choices.get_mut(*index))
                                    && !choice.exists
                                {
                                    choice.selected = !choice.selected;
                                }
                            }
                            KeyCode::Char('a') => {
                                for index in &filtered {
                                    if let Some(choice) = choices.get_mut(*index) {
                                        choice.selected = !choice.exists;
                                    }
                                }
                            }
                            KeyCode::Char('x') => {
                                for index in &filtered {
                                    if let Some(choice) = choices.get_mut(*index) {
                                        choice.selected = false;
                                    }
                                }
                            }
                            _ => {}
                        },
                        DialogFocus::Actions(mut button) => match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => keep_open = false,
                            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up => {
                                focus = DialogFocus::Content;
                            }
                            KeyCode::Left => {
                                button = button.saturating_sub(1);
                                focus = DialogFocus::Actions(button);
                            }
                            KeyCode::Right => {
                                button = (button + 1).min(3);
                                focus = DialogFocus::Actions(button);
                            }
                            KeyCode::Home => focus = DialogFocus::Actions(0),
                            KeyCode::End => focus = DialogFocus::Actions(3),
                            KeyCode::Enter | KeyCode::Char(' ') => match button {
                                0 => {
                                    self.import_discovered(&provider_id, &choices);
                                    return;
                                }
                                1 => {
                                    for choice in &mut choices {
                                        choice.selected = !choice.exists;
                                    }
                                }
                                2 => {
                                    for choice in &mut choices {
                                        choice.selected = false;
                                    }
                                }
                                _ => keep_open = false,
                            },
                            _ => {}
                        },
                    }
                }
                if cursor < scroll {
                    scroll = cursor;
                }
                if keep_open {
                    self.overlay = Some(Overlay::DiscoveryPicker {
                        provider_id,
                        choices,
                        filtered,
                        query,
                        filter_active,
                        cursor,
                        scroll,
                        focus,
                    });
                }
            }
            Overlay::KnownModelsLoading { provider_id } => {
                if key.code == KeyCode::Esc {
                    self.known_models_rx = None;
                    self.set_status(StatusKind::Warning, "已忽略已知模型目录结果");
                } else {
                    self.overlay = Some(Overlay::KnownModelsLoading { provider_id });
                }
            }
            Overlay::KnownModelsPicker {
                provider_id,
                choices,
                mut filtered,
                mut query,
                mut cursor,
                mut scroll,
                mut focus,
            } => {
                let mut keep_open = true;
                match focus {
                    DialogFocus::Content => match key.code {
                        KeyCode::Esc => keep_open = false,
                        KeyCode::Tab | KeyCode::BackTab => focus = DialogFocus::Actions(0),
                        KeyCode::Up => cursor = cursor.saturating_sub(1),
                        KeyCode::Down => {
                            cursor = (cursor + 1).min(filtered.len().saturating_sub(1))
                        }
                        KeyCode::PageUp => cursor = cursor.saturating_sub(10),
                        KeyCode::PageDown => {
                            cursor = (cursor + 10).min(filtered.len().saturating_sub(1))
                        }
                        KeyCode::Home => cursor = 0,
                        KeyCode::End => cursor = filtered.len().saturating_sub(1),
                        KeyCode::Enter => {
                            if let Some(choice) =
                                filtered.get(cursor).and_then(|index| choices.get(*index))
                            {
                                self.import_known_model(&provider_id, choice);
                                return;
                            }
                        }
                        KeyCode::Backspace => {
                            query.pop();
                            filtered = filter_known_models(&choices, &query);
                            cursor = 0;
                            scroll = 0;
                        }
                        KeyCode::Char(character)
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            query.push(character);
                            filtered = filter_known_models(&choices, &query);
                            cursor = 0;
                            scroll = 0;
                        }
                        _ => {}
                    },
                    DialogFocus::Actions(_) => match key.code {
                        KeyCode::Esc => keep_open = false,
                        KeyCode::Tab | KeyCode::BackTab | KeyCode::Up => {
                            focus = DialogFocus::Content
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            if let Some(choice) =
                                filtered.get(cursor).and_then(|index| choices.get(*index))
                            {
                                self.import_known_model(&provider_id, choice);
                                return;
                            }
                        }
                        _ => {}
                    },
                }
                if cursor < scroll {
                    scroll = cursor;
                }
                if keep_open {
                    self.overlay = Some(Overlay::KnownModelsPicker {
                        provider_id,
                        choices,
                        filtered,
                        query,
                        cursor,
                        scroll,
                        focus,
                    });
                }
            }
        }
    }

    fn focus_next(&mut self) {
        self.focus = match self.focus {
            Pane::Providers if self.selected_provider().is_some() => Pane::Models,
            _ => Pane::Providers,
        };
    }

    fn focus_previous(&mut self) {
        self.focus = match self.focus {
            Pane::Models => Pane::Providers,
            Pane::Providers if self.selected_provider().is_some() => Pane::Models,
            Pane::Providers => Pane::Providers,
        };
    }

    fn move_selection(&mut self, amount: isize) {
        match self.focus {
            Pane::Providers => {
                let max = self.visible_providers().len().saturating_sub(1);
                self.provider_cursor = move_index(self.provider_cursor, amount, max);
                self.model_cursor = 0;
            }
            Pane::Models => {
                let max = self.visible_models().len().saturating_sub(1);
                self.model_cursor = move_index(self.model_cursor, amount, max);
            }
        }
    }

    fn move_to_edge(&mut self, end: bool) {
        match self.focus {
            Pane::Providers => {
                self.provider_cursor = if end {
                    self.visible_providers().len().saturating_sub(1)
                } else {
                    0
                };
                self.model_cursor = 0;
            }
            Pane::Models => {
                self.model_cursor = if end {
                    self.visible_models().len().saturating_sub(1)
                } else {
                    0
                };
            }
        }
    }

    fn edit_selected(&mut self) {
        match self.focus {
            Pane::Providers => {
                let Some(provider) = self.selected_provider() else {
                    return;
                };
                let value = self
                    .doc
                    .provider_value(&provider.summary.id)
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                self.overlay = Some(Overlay::Form(FormState::provider(
                    Some(provider.summary.id.clone()),
                    provider.summary.id,
                    &value,
                )));
            }
            Pane::Models => {
                let (Some(provider), Some(model)) =
                    (self.selected_provider(), self.selected_model())
                else {
                    return;
                };
                let value = self
                    .doc
                    .model_value(&provider.summary.id, model.source_index)
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                self.overlay = Some(Overlay::Form(FormState::model(
                    provider.summary.id,
                    Some(model.source_index),
                    &value,
                )));
            }
        }
    }

    fn open_templates(&mut self) {
        if self.ensure_writable() {
            self.overlay = Some(Overlay::Templates {
                selected: 0,
                focus: DialogFocus::Content,
            });
        }
    }

    fn new_provider_from_template(&mut self, info: ProviderTemplateInfo) {
        let id = self.unique_provider_id(info.suggested_id);
        let value = provider_template(info.template);
        self.overlay = Some(Overlay::Form(FormState::provider(None, id, &value)));
    }

    fn new_model(&mut self) {
        if !self.ensure_writable() {
            return;
        }
        let Some(provider) = self.selected_provider() else {
            self.set_status(StatusKind::Warning, "请先新增或选择一个提供商");
            return;
        };
        self.overlay = Some(Overlay::Form(FormState::model(
            provider.summary.id,
            None,
            &json!({"contextWindow": 128000, "maxTokens": 16384}),
        )));
    }

    fn start_known_models(&mut self) {
        if !self.ensure_writable() {
            return;
        }
        let Some(provider) = self.selected_provider() else {
            self.set_status(StatusKind::Warning, "请先选择提供商");
            return;
        };
        let provider_id = provider.summary.id;
        let result_provider_id = provider_id.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = fetch_known_models().map_err(|error| error.to_string());
            let _ = sender.send(KnownModelsResult {
                provider_id: result_provider_id,
                result,
            });
        });
        self.known_models_rx = Some(receiver);
        self.overlay = Some(Overlay::KnownModelsLoading { provider_id });
        self.set_status(StatusKind::Info, "正在获取最新已知模型数据...");
    }

    fn poll_known_models(&mut self) {
        let result = match self.known_models_rx.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => {
                self.known_models_rx = None;
                if matches!(self.overlay, Some(Overlay::KnownModelsLoading { .. })) {
                    self.overlay = None;
                    self.set_status(StatusKind::Error, "已知模型目录任务意外结束");
                }
                None
            }
            Some(Err(TryRecvError::Empty)) | None => None,
        };
        let Some(result) = result else { return };
        self.known_models_rx = None;
        match result.result {
            Ok(models) => {
                let existing: std::collections::HashSet<_> = self
                    .doc
                    .models(&result.provider_id)
                    .into_iter()
                    .map(|model| model.id)
                    .collect();
                let choices = models
                    .into_iter()
                    .map(|model| KnownModelChoice {
                        exists: existing.contains(&model.id),
                        model,
                    })
                    .collect::<Vec<_>>();
                let filtered = (0..choices.len()).collect();
                let count = choices.len();
                self.overlay = Some(Overlay::KnownModelsPicker {
                    provider_id: result.provider_id,
                    choices,
                    filtered,
                    query: String::new(),
                    cursor: 0,
                    scroll: 0,
                    focus: DialogFocus::Content,
                });
                self.set_status(StatusKind::Success, format!("已载入 {count} 个已知模型"));
            }
            Err(error) => {
                self.overlay = None;
                self.set_status(StatusKind::Error, error);
            }
        }
    }

    fn import_known_model(&mut self, provider_id: &str, choice: &KnownModelChoice) {
        if choice.exists {
            self.set_status(
                StatusKind::Warning,
                format!("模型 {} 已存在", choice.model.id),
            );
            return;
        }
        let previous = self.doc.root().clone();
        if let Some(index) = self.doc.push_model(provider_id, choice.model.value.clone()) {
            self.record_snapshot(previous);
            self.search.clear();
            self.select_provider_id(provider_id);
            self.select_model_source_index(index);
            self.focus = Pane::Models;
            self.set_status(
                StatusKind::Success,
                format!("已按已知参数导入 {}", choice.model.id),
            );
        } else {
            self.set_status(StatusKind::Error, "提供商 models 字段不是数组");
        }
    }

    fn apply_form(&mut self, form: &FormState) -> Result<(), String> {
        if !self.ensure_writable() {
            return Err("只读模式下不能修改配置".into());
        }
        let existing = match &form.target {
            crate::editor::FormTarget::Provider { original_id } => original_id
                .as_deref()
                .and_then(|id| self.doc.provider_value(id))
                .cloned(),
            crate::editor::FormTarget::Model {
                provider_id,
                original_index,
            } => original_index
                .and_then(|index| self.doc.model_value(provider_id, index))
                .cloned(),
        };
        match form.submit(existing.as_ref())? {
            FormSubmission::Provider {
                original_id,
                id,
                value,
            } => {
                if self.doc.provider_value(&id).is_some()
                    && original_id.as_deref() != Some(id.as_str())
                {
                    return Err(format!("提供商 ID {id} 已存在"));
                }
                let previous = self.doc.root().clone();
                if !self
                    .doc
                    .upsert_provider(original_id.as_deref(), id.clone(), value)
                {
                    return Err("providers 字段不是对象，无法安全修改".into());
                }
                self.record_snapshot(previous);
                self.search.clear();
                self.select_provider_id(&id);
                self.focus = Pane::Providers;
                self.set_status(StatusKind::Success, format!("已更新提供商 {id}"));
            }
            FormSubmission::Model {
                provider_id,
                original_index,
                value,
            } => {
                let model_id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let duplicate = self
                    .doc
                    .models(&provider_id)
                    .iter()
                    .enumerate()
                    .any(|(index, model)| model.id == model_id && original_index != Some(index));
                if duplicate {
                    return Err(format!("模型 ID {model_id} 在该提供商中已存在"));
                }
                let previous = self.doc.root().clone();
                let target_index = if let Some(index) = original_index {
                    if !self.doc.replace_model(&provider_id, index, value) {
                        return Err("原模型已不存在，请关闭表单后重试".into());
                    }
                    index
                } else {
                    self.doc
                        .push_model(&provider_id, value)
                        .ok_or_else(|| "提供商 models 字段不是数组".to_string())?
                };
                self.record_snapshot(previous);
                self.search.clear();
                self.select_provider_id(&provider_id);
                self.select_model_source_index(target_index);
                self.focus = Pane::Models;
                self.set_status(StatusKind::Success, format!("已更新模型 {model_id}"));
            }
        }
        Ok(())
    }

    fn confirm_delete(&mut self) {
        if !self.ensure_writable() {
            return;
        }
        match self.focus {
            Pane::Providers => {
                let Some(provider) = self.selected_provider() else {
                    return;
                };
                self.overlay = Some(Overlay::Confirm {
                    title: "删除提供商".into(),
                    message: format!(
                        "删除 {} 及其 {} 个自定义模型？",
                        provider.summary.id, provider.summary.model_count
                    ),
                    action: ConfirmAction::DeleteProvider {
                        provider_id: provider.summary.id,
                    },
                    selected_button: 0,
                });
            }
            Pane::Models => {
                let (Some(provider), Some(model)) =
                    (self.selected_provider(), self.selected_model())
                else {
                    return;
                };
                self.overlay = Some(Overlay::Confirm {
                    title: "删除模型".into(),
                    message: format!("从 {} 删除模型 {}？", provider.summary.id, model.summary.id),
                    action: ConfirmAction::DeleteModel {
                        provider_id: provider.summary.id,
                        model_index: model.source_index,
                        model_id: model.summary.id,
                    },
                    selected_button: 0,
                });
            }
        }
    }

    fn duplicate_selected(&mut self) {
        if !self.ensure_writable() {
            return;
        }
        match self.focus {
            Pane::Providers => {
                let Some(provider) = self.selected_provider() else {
                    return;
                };
                let Some(value) = self.doc.provider_value(&provider.summary.id).cloned() else {
                    return;
                };
                let new_id = self.unique_provider_id(&format!("{}-copy", provider.summary.id));
                let previous = self.doc.root().clone();
                if !self.doc.upsert_provider(None, new_id.clone(), value) {
                    self.set_status(StatusKind::Error, "providers 字段不是对象，无法安全修改");
                    return;
                }
                self.record_snapshot(previous);
                self.search.clear();
                self.select_provider_id(&new_id);
                self.set_status(StatusKind::Success, format!("已复制为 {new_id}"));
            }
            Pane::Models => {
                let (Some(provider), Some(model)) =
                    (self.selected_provider(), self.selected_model())
                else {
                    return;
                };
                let Some(mut value) = self
                    .doc
                    .model_value(&provider.summary.id, model.source_index)
                    .cloned()
                else {
                    return;
                };
                let new_id = self
                    .unique_model_id(&provider.summary.id, &format!("{}-copy", model.summary.id));
                if let Some(object) = value.as_object_mut() {
                    object.insert("id".into(), Value::String(new_id.clone()));
                    if let Some(name) = object.get("name").and_then(Value::as_str) {
                        let copy_name = format!("{name} Copy");
                        object.insert("name".into(), Value::String(copy_name));
                    }
                }
                let previous = self.doc.root().clone();
                if let Some(index) = self.doc.push_model(&provider.summary.id, value) {
                    self.record_snapshot(previous);
                    self.search.clear();
                    self.select_model_source_index(index);
                    self.set_status(StatusKind::Success, format!("已复制为 {new_id}"));
                } else {
                    self.set_status(StatusKind::Error, "提供商 models 字段不是数组");
                }
            }
        }
    }

    fn start_discovery(&mut self) {
        let Some(provider) = self.selected_provider() else {
            self.set_status(StatusKind::Warning, "请先选择提供商");
            return;
        };
        let Some(value) = self.doc.provider_value(&provider.summary.id) else {
            return;
        };
        let config_dir = self
            .doc
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let request =
            match DiscoveryRequest::from_provider(provider.summary.id.clone(), value, config_dir) {
                Ok(request) => request,
                Err(error) => {
                    self.set_status(StatusKind::Error, error.to_string());
                    return;
                }
            };
        let provider_id = request.provider_id().to_owned();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = discover_models(&request)
                .map(|mut models| {
                    if let Ok(known) = fetch_known_models() {
                        let known = known
                            .into_iter()
                            .map(|model| (model.id.clone(), model.value))
                            .collect::<std::collections::HashMap<_, _>>();
                        for model in &mut models {
                            model.config = known.get(&model.id).cloned();
                        }
                    }
                    models
                })
                .map_err(|error| error.to_string());
            let _ = sender.send(DiscoveryResult {
                provider_id,
                result,
            });
        });
        self.discovery_rx = Some(receiver);
        self.overlay = Some(Overlay::DiscoveryLoading {
            provider_id: provider.summary.id,
        });
        self.set_status(StatusKind::Info, "正在读取远程模型目录...");
    }

    fn import_discovered(&mut self, provider_id: &str, choices: &[DiscoveryChoice]) {
        if !self.ensure_writable() {
            return;
        }
        let selected = choices
            .iter()
            .filter(|choice| choice.selected && !choice.exists)
            .collect::<Vec<_>>();
        if selected.is_empty() {
            self.set_status(StatusKind::Warning, "没有选择待导入模型");
            return;
        }
        let previous = self.doc.root().clone();
        let mut imported = 0;
        let mut last_index = None;
        for choice in selected {
            let mut model = choice
                .model
                .config
                .clone()
                .unwrap_or_else(|| json!({ "id": choice.model.id }));
            if let Some(name) = &choice.model.name
                && name != &choice.model.id
            {
                model["name"] = Value::String(name.clone());
            }
            if let Some(index) = self.doc.push_model(provider_id, model) {
                imported += 1;
                last_index = Some(index);
            }
        }
        if imported == 0 {
            self.set_status(StatusKind::Error, "提供商 models 字段不是数组");
            return;
        }
        self.record_snapshot(previous);
        self.search.clear();
        self.select_provider_id(provider_id);
        if let Some(index) = last_index {
            self.select_model_source_index(index);
        }
        self.focus = Pane::Models;
        self.set_status(StatusKind::Success, format!("已导入 {imported} 个模型"));
    }

    fn execute_confirm(&mut self, action: ConfirmAction) {
        match action {
            ConfirmAction::DeleteProvider { provider_id } => {
                let previous = self.doc.root().clone();
                if self.doc.remove_provider(&provider_id).is_some() {
                    self.record_snapshot(previous);
                    self.normalize_selection();
                    self.set_status(StatusKind::Success, format!("已删除提供商 {provider_id}"));
                } else {
                    self.set_status(StatusKind::Error, "提供商已不存在");
                }
            }
            ConfirmAction::DeleteModel {
                provider_id,
                model_index,
                model_id,
            } => {
                let previous = self.doc.root().clone();
                if self.doc.remove_model(&provider_id, model_index).is_some() {
                    self.record_snapshot(previous);
                    self.normalize_selection();
                    self.set_status(StatusKind::Success, format!("已删除模型 {model_id}"));
                } else {
                    self.set_status(StatusKind::Error, "模型已不存在");
                }
            }
            ConfirmAction::Quit => self.should_quit = true,
            ConfirmAction::Reload => self.reload(),
            ConfirmAction::ForceSave => self.save(true),
        }
    }

    fn request_quit(&mut self) {
        if self.is_dirty() {
            self.overlay = Some(Overlay::Confirm {
                title: "放弃未保存更改".into(),
                message: "退出后，本次尚未保存的修改会丢失。".into(),
                action: ConfirmAction::Quit,
                selected_button: 0,
            });
        } else {
            self.should_quit = true;
        }
    }

    fn request_reload(&mut self) {
        if self.is_dirty() {
            self.overlay = Some(Overlay::Confirm {
                title: "重新载入配置".into(),
                message: "重新载入会放弃当前未保存的修改。".into(),
                action: ConfirmAction::Reload,
                selected_button: 0,
            });
        } else {
            self.reload();
        }
    }

    fn reload(&mut self) {
        match ConfigDocument::load(self.doc.path()) {
            Ok(doc) => {
                self.saved_root = doc.root().clone();
                self.doc = doc;
                self.undo.clear();
                self.redo.clear();
                self.search.clear();
                self.normalize_selection();
                self.set_status(StatusKind::Success, "已从磁盘重新载入");
            }
            Err(error) => self.set_status(StatusKind::Error, error.to_string()),
        }
    }

    fn save(&mut self, force: bool) {
        if !self.ensure_writable() {
            return;
        }
        let diagnostics = self.doc.validate();
        let errors = diagnostics
            .iter()
            .filter(|item| item.severity == Severity::Error)
            .count();
        if errors > 0 {
            self.overlay = Some(Overlay::Diagnostics { scroll: 0 });
            self.set_status(StatusKind::Error, format!("存在 {errors} 个错误，未保存"));
            return;
        }
        match self.doc.save(self.create_backup || force, force) {
            Ok(outcome) => {
                self.saved_root = self.doc.root().clone();
                let message = outcome.backup.as_ref().map_or_else(
                    || format!("已保存 {} 字节", outcome.bytes),
                    |path| {
                        format!(
                            "已保存；备份 {}",
                            path.file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("已创建")
                        )
                    },
                );
                self.set_status(StatusKind::Success, message);
            }
            Err(ConfigError::ExternalChange) if !force => {
                self.overlay = Some(Overlay::Confirm {
                    title: "磁盘文件已变化".into(),
                    message: "文件在载入后被其他程序修改。强制保存会先备份当前磁盘版本。".into(),
                    action: ConfirmAction::ForceSave,
                    selected_button: 0,
                });
            }
            Err(error) => self.set_status(StatusKind::Error, error.to_string()),
        }
    }

    fn undo(&mut self) {
        if !self.ensure_writable() {
            return;
        }
        let Some(previous) = self.undo.pop() else {
            self.set_status(StatusKind::Info, "没有可撤销的修改");
            return;
        };
        self.redo.push(self.doc.root().clone());
        self.doc.replace_root(previous);
        self.normalize_selection();
        self.set_status(StatusKind::Info, "已撤销");
    }

    fn redo(&mut self) {
        if !self.ensure_writable() {
            return;
        }
        let Some(next) = self.redo.pop() else {
            self.set_status(StatusKind::Info, "没有可重做的修改");
            return;
        };
        self.undo.push(self.doc.root().clone());
        self.doc.replace_root(next);
        self.normalize_selection();
        self.set_status(StatusKind::Info, "已重做");
    }

    fn record_snapshot(&mut self, snapshot: Value) {
        if self.undo.len() == HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(snapshot);
        self.redo.clear();
    }

    fn ensure_writable(&mut self) -> bool {
        if self.read_only {
            self.set_status(StatusKind::Warning, "只读模式下不能修改或保存配置");
            false
        } else {
            true
        }
    }

    fn normalize_selection(&mut self) {
        let providers = self.visible_providers();
        self.provider_cursor = self.provider_cursor.min(providers.len().saturating_sub(1));
        if providers.is_empty() {
            self.provider_cursor = 0;
            self.model_cursor = 0;
            self.focus = Pane::Providers;
            return;
        }
        let models = self.visible_models();
        self.model_cursor = self.model_cursor.min(models.len().saturating_sub(1));
    }

    fn select_provider_id(&mut self, provider_id: &str) {
        self.normalize_selection();
        if let Some(index) = self
            .visible_providers()
            .iter()
            .position(|provider| provider.summary.id == provider_id)
        {
            self.provider_cursor = index;
            self.model_cursor = 0;
        }
    }

    fn select_model_source_index(&mut self, source_index: usize) {
        if let Some(index) = self
            .visible_models()
            .iter()
            .position(|model| model.source_index == source_index)
        {
            self.model_cursor = index;
        }
    }

    fn provider_matches_query(&self, provider: &ProviderSummary) -> bool {
        if self.search.trim().is_empty() {
            return true;
        }
        let matcher = SkimMatcherV2::default().ignore_case();
        provider_search_text(provider)
            .iter()
            .any(|text| matcher.fuzzy_match(text, self.search.trim()).is_some())
    }

    fn unique_provider_id(&self, base: &str) -> String {
        unique_id(base, |candidate| {
            self.doc.provider_value(candidate).is_some()
        })
    }

    fn unique_model_id(&self, provider_id: &str, base: &str) -> String {
        let existing: std::collections::HashSet<_> = self
            .doc
            .models(provider_id)
            .into_iter()
            .map(|model| model.id)
            .collect();
        unique_id(base, |candidate| existing.contains(candidate))
    }

    fn set_status(&mut self, kind: StatusKind, text: impl Into<String>) {
        self.status = StatusMessage {
            kind,
            text: text.into(),
        };
    }
}

fn provider_search_text(provider: &ProviderSummary) -> Vec<&str> {
    let mut values = vec![provider.id.as_str()];
    if let Some(api) = provider.api.as_deref() {
        values.push(api);
    }
    if let Some(url) = provider.base_url.as_deref() {
        values.push(url);
    }
    values
}

fn model_search_text(model: &ModelSummary) -> Vec<String> {
    let mut values = vec![model.id.clone()];
    if let Some(name) = &model.name {
        values.push(name.clone());
    }
    if let Some(api) = &model.api {
        values.push(api.clone());
    }
    values
}

fn filter_known_models(choices: &[KnownModelChoice], query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..choices.len()).collect();
    }
    let matcher = SkimMatcherV2::default().ignore_case();
    let mut matched = choices
        .iter()
        .enumerate()
        .filter_map(|(index, choice)| {
            [
                Some(choice.model.id.as_str()),
                choice.model.name.as_deref(),
                Some(choice.model.family.as_str()),
            ]
            .into_iter()
            .flatten()
            .filter_map(|text| matcher.fuzzy_match(text, query.trim()))
            .max()
            .map(|score| (Reverse(score), index))
        })
        .collect::<Vec<_>>();
    matched.sort_by_key(|item| *item);
    matched.into_iter().map(|(_, index)| index).collect()
}

fn filter_discovered_models(choices: &[DiscoveryChoice], query: &str) -> Vec<usize> {
    let query = query.trim().to_ascii_lowercase();
    choices
        .iter()
        .enumerate()
        .filter(|(_, choice)| {
            query.is_empty() || choice.model.id.to_ascii_lowercase().contains(&query)
        })
        .map(|(index, _)| index)
        .collect()
}

fn move_index(current: usize, amount: isize, max: usize) -> usize {
    if amount.is_negative() {
        current.saturating_sub(amount.unsigned_abs())
    } else {
        current.saturating_add(amount as usize).min(max)
    }
}

fn unique_id(base: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(base) {
        return base.to_owned();
    }
    for suffix in 2..10_000 {
        let candidate = format!("{base}-{suffix}");
        if !exists(&candidate) {
            return candidate;
        }
    }
    format!("{base}-{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::ConfigDocument;

    fn press(app: &mut App, code: KeyCode) {
        app.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn app() -> App {
        App::new(
            ConfigDocument::from_value(
                PathBuf::from("/tmp/models.json"),
                json!({
                    "providers": {
                        "alpha": {
                            "baseUrl":"https://example.com/v1",
                            "api":"openai-completions",
                            "models":[{"id":"qwen-coder"},{"id":"llama"}]
                        },
                        "beta": {
                            "baseUrl":"https://example.org/v1",
                            "api":"openai-completions",
                            "models":[{"id":"deepseek-reasoner"}]
                        }
                    }
                }),
            ),
            false,
            true,
        )
    }

    #[test]
    fn fuzzy_search_finds_provider_through_model() {
        let mut app = app();
        app.search = "reason".into();
        app.normalize_selection();
        let providers = app.visible_providers();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].summary.id, "beta");
        assert_eq!(app.visible_models()[0].summary.id, "deepseek-reasoner");
    }

    #[test]
    fn undo_and_redo_restore_document() {
        let mut app = app();
        let original = app.doc.root().clone();
        app.doc.remove_provider("alpha");
        app.record_snapshot(original.clone());
        assert_ne!(app.doc.root(), &original);
        app.undo();
        assert_eq!(app.doc.root(), &original);
        app.redo();
        assert!(app.doc.provider_value("alpha").is_none());
    }

    #[test]
    fn mouse_click_selects_and_double_click_opens_editor() {
        let mut app = app();
        app.mouse_select_list(Pane::Providers, 1, false);
        assert_eq!(app.provider_cursor, 1);
        assert_eq!(app.focus, Pane::Providers);
        assert!(app.overlay.is_none());

        app.mouse_select_list(Pane::Providers, 1, false);
        assert!(matches!(app.overlay, Some(Overlay::Form(_))));
    }

    #[test]
    fn mouse_wheel_moves_the_target_list() {
        let mut app = app();
        app.mouse_scroll_list(Pane::Providers, 3);
        assert_eq!(app.provider_cursor, 1);
        app.provider_cursor = 0;
        app.focus = Pane::Models;
        app.model_cursor = 0;
        app.mouse_scroll_list(Pane::Models, 3);
        assert_eq!(app.model_cursor, 1);
    }

    #[test]
    fn template_dialog_switches_between_content_and_actions() {
        let mut app = app();
        app.open_templates();
        assert!(matches!(
            app.overlay,
            Some(Overlay::Templates {
                focus: DialogFocus::Content,
                ..
            })
        ));

        press(&mut app, KeyCode::Tab);
        assert!(matches!(
            app.overlay,
            Some(Overlay::Templates {
                focus: DialogFocus::Actions(0),
                ..
            })
        ));
        press(&mut app, KeyCode::Right);
        assert!(matches!(
            app.overlay,
            Some(Overlay::Templates {
                focus: DialogFocus::Actions(1),
                ..
            })
        ));
        press(&mut app, KeyCode::Up);
        assert!(matches!(
            app.overlay,
            Some(Overlay::Templates {
                focus: DialogFocus::Content,
                ..
            })
        ));

        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Down);
        assert!(matches!(
            app.overlay,
            Some(Overlay::Templates {
                focus: DialogFocus::Actions(0),
                ..
            })
        ));
    }

    #[test]
    fn enter_directly_chooses_provider_template() {
        let mut app = app();
        app.open_templates();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        let Some(Overlay::Form(form)) = &app.overlay else {
            panic!("Enter on template content should open the provider form")
        };
        assert!(form.title.contains("新增提供商"));
    }

    #[test]
    fn discovery_dialog_actions_use_horizontal_focus() {
        let mut app = app();
        app.overlay = Some(Overlay::DiscoveryPicker {
            provider_id: "alpha".into(),
            choices: vec![DiscoveryChoice {
                model: DiscoveredModel {
                    id: "new-model".into(),
                    name: None,
                    config: None,
                },
                selected: false,
                exists: false,
            }],
            filtered: vec![0],
            query: String::new(),
            filter_active: false,
            cursor: 0,
            scroll: 0,
            focus: DialogFocus::Content,
        });

        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Enter);
        let Some(Overlay::DiscoveryPicker { choices, focus, .. }) = &app.overlay else {
            panic!("discovery picker should remain open")
        };
        assert!(choices[0].selected);
        assert_eq!(*focus, DialogFocus::Actions(1));

        press(&mut app, KeyCode::Tab);
        assert!(matches!(
            app.overlay,
            Some(Overlay::DiscoveryPicker {
                focus: DialogFocus::Content,
                ..
            })
        ));
    }

    #[test]
    fn discovery_dialog_filters_by_model_id() {
        let mut app = app();
        app.overlay = Some(Overlay::DiscoveryPicker {
            provider_id: "alpha".into(),
            choices: vec![
                DiscoveryChoice {
                    model: DiscoveredModel {
                        id: "gpt-5".into(),
                        name: None,
                        config: None,
                    },
                    selected: true,
                    exists: false,
                },
                DiscoveryChoice {
                    model: DiscoveredModel {
                        id: "claude-sonnet".into(),
                        name: None,
                        config: None,
                    },
                    selected: true,
                    exists: false,
                },
            ],
            filtered: vec![0, 1],
            query: String::new(),
            filter_active: false,
            cursor: 0,
            scroll: 0,
            focus: DialogFocus::Content,
        });

        press(&mut app, KeyCode::Char('/'));
        for character in "claude".chars() {
            press(&mut app, KeyCode::Char(character));
        }
        let Some(Overlay::DiscoveryPicker {
            filtered,
            query,
            filter_active,
            ..
        }) = &app.overlay
        else {
            panic!("discovery picker should remain open")
        };
        assert_eq!(query, "claude");
        assert_eq!(filtered, &[1]);
        assert!(*filter_active);

        press(&mut app, KeyCode::Esc);
        assert!(matches!(
            app.overlay,
            Some(Overlay::DiscoveryPicker {
                filter_active: false,
                ..
            })
        ));
    }

    #[test]
    fn enter_directly_imports_discovery_selection() {
        let mut app = app();
        app.overlay = Some(Overlay::DiscoveryPicker {
            provider_id: "alpha".into(),
            choices: vec![DiscoveryChoice {
                model: DiscoveredModel {
                    id: "direct-import".into(),
                    name: None,
                    config: None,
                },
                selected: true,
                exists: false,
            }],
            filtered: vec![0],
            query: String::new(),
            filter_active: false,
            cursor: 0,
            scroll: 0,
            focus: DialogFocus::Content,
        });

        press(&mut app, KeyCode::Enter);
        assert!(app.overlay.is_none());
        assert!(
            app.doc
                .models("alpha")
                .iter()
                .any(|model| model.id == "direct-import")
        );
    }

    #[test]
    fn unique_ids_are_predictable() {
        let app = app();
        assert_eq!(app.unique_provider_id("alpha"), "alpha-2");
        assert_eq!(app.unique_model_id("alpha", "llama"), "llama-2");
    }
}
