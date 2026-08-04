use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Wrap,
};
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, DialogFocus, Overlay, Pane, StatusKind};
use crate::config::{CredentialHint, Diagnostic, Severity};
use crate::editor::{FieldKind, FormState, PROVIDER_TEMPLATES};

const BG: Color = Color::Rgb(16, 18, 20);
const PANEL: Color = Color::Rgb(22, 24, 27);
const SURFACE: Color = Color::Rgb(31, 34, 37);
const BORDER: Color = Color::Rgb(75, 79, 82);
const TEXT: Color = Color::Rgb(224, 226, 228);
const MUTED: Color = Color::Rgb(140, 144, 147);
const CYAN: Color = Color::Rgb(55, 190, 200);
const GREEN: Color = Color::Rgb(95, 190, 120);
const YELLOW: Color = Color::Rgb(225, 180, 70);
const RED: Color = Color::Rgb(225, 90, 90);
const MAGENTA: Color = Color::Rgb(190, 120, 200);
const BLUE: Color = Color::Rgb(90, 145, 220);

#[derive(Debug, Clone, Copy)]
struct ScreenRegions {
    header: Rect,
    workspace: Rect,
    status: Rect,
    commands: Rect,
}

#[derive(Debug, Clone, Copy)]
struct WorkspaceRegions {
    providers: Option<Rect>,
    models: Option<Rect>,
    details: Rect,
}

fn screen_regions(area: Rect) -> ScreenRegions {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
    ScreenRegions {
        header: rows[0],
        workspace: rows[1],
        status: rows[2],
        commands: rows[3],
    }
}

fn workspace_regions(area: Rect, focus: Pane) -> WorkspaceRegions {
    if area.width >= 110 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(27),
                Constraint::Percentage(34),
                Constraint::Min(36),
            ])
            .split(area);
        WorkspaceRegions {
            providers: Some(columns[0]),
            models: Some(columns[1]),
            details: columns[2],
        }
    } else if area.width >= 76 {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(61), Constraint::Min(6)])
            .split(area);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(rows[0]);
        WorkspaceRegions {
            providers: Some(columns[0]),
            models: Some(columns[1]),
            details: rows[1],
        }
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(62), Constraint::Min(5)])
            .split(area);
        WorkspaceRegions {
            providers: (focus == Pane::Providers).then_some(rows[0]),
            models: (focus == Pane::Models).then_some(rows[0]),
            details: rows[1],
        }
    }
}

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);

    if area.width < 42 || area.height < 12 {
        draw_too_small(frame, area);
        return;
    }

    let regions = screen_regions(area);
    draw_header(frame, app, regions.header);
    draw_workspace(frame, app, regions.workspace);
    draw_status(frame, app, regions.status);
    draw_commands(frame, app, regions.commands);

    if let Some(overlay) = &app.overlay {
        draw_overlay(frame, app, overlay, area);
    }
}

#[derive(Debug, Clone, Copy)]
enum MouseAction {
    Key(KeyCode, KeyModifiers),
    ActivateDialogButton(usize),
    FocusPane(Pane),
    SelectList {
        pane: Pane,
        index: usize,
        activate: bool,
    },
    ScrollList {
        pane: Pane,
        amount: isize,
    },
    ActivateSearch,
    ClearSearch,
    SelectTemplate {
        index: usize,
        activate: bool,
    },
    SelectFormField {
        index: usize,
        value_column: usize,
        activate: Option<isize>,
    },
    SelectDiscovered {
        index: usize,
        toggle: bool,
    },
    ScrollOverlay(isize),
}

pub fn handle_mouse(app: &mut App, event: MouseEvent, terminal: Rect) {
    if terminal.width < 42
        || terminal.height < 12
        || matches!(event.kind, MouseEventKind::Down(MouseButton::Middle))
    {
        return;
    }
    let action = if app.overlay.is_some() {
        overlay_mouse_action(app, event, terminal)
    } else {
        base_mouse_action(app, event, terminal)
    };
    if let Some(action) = action {
        apply_mouse_action(app, action);
    }
}

fn base_mouse_action(app: &App, event: MouseEvent, terminal: Rect) -> Option<MouseAction> {
    let regions = screen_regions(terminal);
    let workspace = workspace_regions(regions.workspace, app.focus);
    let point = Position::new(event.column, event.row);

    match event.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let amount = if event.kind == MouseEventKind::ScrollUp {
                -3
            } else {
                3
            };
            if workspace.providers.is_some_and(|area| area.contains(point)) {
                return Some(MouseAction::ScrollList {
                    pane: Pane::Providers,
                    amount,
                });
            }
            if workspace.models.is_some_and(|area| area.contains(point)) {
                return Some(MouseAction::ScrollList {
                    pane: Pane::Models,
                    amount,
                });
            }
            return None;
        }
        MouseEventKind::Down(button) => {
            if event.row == regions.header.y + 2 {
                return match button {
                    MouseButton::Left => Some(MouseAction::ActivateSearch),
                    MouseButton::Right => Some(MouseAction::ClearSearch),
                    _ => None,
                };
            }
            if button == MouseButton::Left && regions.status.contains(point) {
                return Some(MouseAction::Key(KeyCode::Char('v'), KeyModifiers::NONE));
            }
            if button == MouseButton::Left && regions.commands.contains(point) {
                return command_at(app, regions.commands, event.column)
                    .map(|command| MouseAction::Key(command.code, command.modifiers));
            }
            if let Some(area) = workspace.providers
                && area.contains(point)
            {
                return if let Some(index) = list_index_at(
                    app.visible_providers().len(),
                    area,
                    app.provider_cursor,
                    event.row,
                ) {
                    Some(MouseAction::SelectList {
                        pane: Pane::Providers,
                        index,
                        activate: button == MouseButton::Right,
                    })
                } else {
                    Some(MouseAction::FocusPane(Pane::Providers))
                };
            }
            if let Some(area) = workspace.models
                && area.contains(point)
            {
                return if let Some(index) = list_index_at(
                    app.visible_models().len(),
                    area,
                    app.model_cursor,
                    event.row,
                ) {
                    Some(MouseAction::SelectList {
                        pane: Pane::Models,
                        index,
                        activate: button == MouseButton::Right,
                    })
                } else {
                    Some(MouseAction::FocusPane(Pane::Models))
                };
            }
        }
        _ => {}
    }
    None
}

fn overlay_mouse_action(app: &App, event: MouseEvent, terminal: Rect) -> Option<MouseAction> {
    let overlay = app.overlay.as_ref()?;
    let point = Position::new(event.column, event.row);
    let scroll = match event.kind {
        MouseEventKind::ScrollUp => Some(-3),
        MouseEventKind::ScrollDown => Some(3),
        _ => None,
    };

    match overlay {
        Overlay::Help { .. } => {
            let area = modal_rect(terminal, 78, 30, 84, 84);
            if let Some(amount) = scroll
                && area.contains(point)
            {
                return Some(MouseAction::ScrollOverlay(amount));
            }
            if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
                && !area.contains(point)
            {
                return Some(MouseAction::Key(KeyCode::Esc, KeyModifiers::NONE));
            }
        }
        Overlay::Diagnostics { .. } => {
            let area = modal_rect(terminal, 90, 28, 90, 82);
            if let Some(amount) = scroll
                && area.contains(point)
            {
                return Some(MouseAction::ScrollOverlay(amount));
            }
            if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
                && !area.contains(point)
            {
                return Some(MouseAction::Key(KeyCode::Esc, KeyModifiers::NONE));
            }
        }
        Overlay::Templates { selected, .. } => {
            let area = modal_rect(terminal, 72, 20, 82, 72);
            if let Some(amount) = scroll
                && area.contains(point)
            {
                return Some(MouseAction::ScrollOverlay(amount));
            }
            let inner = modal_block("").inner(area);
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(4), Constraint::Length(1)])
                .split(inner);
            if let MouseEventKind::Down(button) = event.kind {
                let buttons = [
                    ActionButton {
                        label: "选择",
                        color: GREEN,
                    },
                    ActionButton {
                        label: "取消",
                        color: MUTED,
                    },
                ];
                if let Some(index) = action_button_at(rows[1], &buttons, point) {
                    return Some(MouseAction::ActivateDialogButton(index));
                }
                if rows[0].contains(point) {
                    let visible = (rows[0].height as usize / 2).max(1);
                    let offset = list_view_offset(PROVIDER_TEMPLATES.len(), visible, *selected);
                    let index = offset + (event.row - rows[0].y) as usize / 2;
                    if index < PROVIDER_TEMPLATES.len() {
                        return Some(MouseAction::SelectTemplate {
                            index,
                            activate: button == MouseButton::Right,
                        });
                    }
                }
            }
        }
        Overlay::Confirm { message, .. } => {
            let width = (message.width() as u16 + 8).clamp(44, 76);
            let area = modal_rect(terminal, width, 9, 90, 60);
            if let MouseEventKind::Down(MouseButton::Left) = event.kind {
                let inner = modal_block("").inner(area);
                let rows = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(3), Constraint::Length(1)])
                    .split(inner);
                let buttons = [
                    ActionButton {
                        label: "确认",
                        color: RED,
                    },
                    ActionButton {
                        label: "取消",
                        color: MUTED,
                    },
                ];
                if let Some(index) = action_button_at(rows[1], &buttons, point) {
                    return Some(MouseAction::ActivateDialogButton(index));
                }
                if !area.contains(point) {
                    return Some(MouseAction::Key(KeyCode::Esc, KeyModifiers::NONE));
                }
            }
        }
        Overlay::Form(form) => {
            let preferred_height = (form.fields.len() as u16 + 7).clamp(15, 28);
            let area = modal_rect(terminal, 98, preferred_height, 94, 92);
            if let Some(amount) = scroll
                && area.contains(point)
            {
                return Some(MouseAction::ScrollOverlay(amount));
            }
            if let MouseEventKind::Down(button) = event.kind {
                let inner = modal_block("").inner(area);
                let rows = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Min(4),
                        Constraint::Length(3),
                        Constraint::Length(1),
                    ])
                    .split(inner);
                let buttons = [
                    ActionButton {
                        label: "确定",
                        color: GREEN,
                    },
                    ActionButton {
                        label: "取消",
                        color: MUTED,
                    },
                ];
                if let Some(index) = action_button_at(rows[2], &buttons, point) {
                    return Some(MouseAction::Key(
                        if index == 0 {
                            KeyCode::Char('s')
                        } else {
                            KeyCode::Esc
                        },
                        if index == 0 {
                            KeyModifiers::CONTROL
                        } else {
                            KeyModifiers::NONE
                        },
                    ));
                }
                if rows[0].contains(point) {
                    let start = form_field_start(form, rows[0].height as usize);
                    let index = start + (event.row - rows[0].y) as usize;
                    if index < form.fields.len() {
                        let label_width = if rows[0].width >= 72 { 20 } else { 14 };
                        let label_width = label_width.min(rows[0].width.saturating_sub(4));
                        let value_x = rows[0].x.saturating_add(label_width);
                        let value_width = rows[0].right().saturating_sub(value_x);
                        let horizontal_scroll = if form.fields_focused()
                            && index == form.selected
                            && form.fields[index].is_editable_text()
                        {
                            let available = value_width.saturating_sub(1) as usize;
                            form.cursor_display_width()
                                .saturating_sub(available.saturating_sub(1))
                        } else {
                            0
                        };
                        let value_column = (event.column.saturating_sub(value_x) as usize)
                            .saturating_add(horizontal_scroll);
                        let activate = (event.column >= value_x)
                            .then_some(if button == MouseButton::Right { -1 } else { 1 });
                        return Some(MouseAction::SelectFormField {
                            index,
                            value_column,
                            activate,
                        });
                    }
                }
            }
        }
        Overlay::DiscoveryLoading { .. } => {
            let area = modal_rect(terminal, 54, 9, 84, 54);
            if matches!(event.kind, MouseEventKind::Down(MouseButton::Right))
                || matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
                    && !area.contains(point)
            {
                return Some(MouseAction::Key(KeyCode::Esc, KeyModifiers::NONE));
            }
        }
        Overlay::DiscoveryPicker {
            filtered,
            cursor,
            scroll: current_scroll,
            ..
        } => {
            let area = modal_rect(terminal, 88, 28, 92, 88);
            if let Some(amount) = scroll
                && area.contains(point)
            {
                return Some(MouseAction::ScrollOverlay(amount));
            }
            if let MouseEventKind::Down(button) = event.kind {
                let inner = modal_block("").inner(area);
                let rows = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(2),
                        Constraint::Min(4),
                        Constraint::Length(1),
                    ])
                    .split(inner);
                let buttons = [
                    ActionButton {
                        label: "导入",
                        color: GREEN,
                    },
                    ActionButton {
                        label: "全选",
                        color: TEXT,
                    },
                    ActionButton {
                        label: "清空",
                        color: YELLOW,
                    },
                    ActionButton {
                        label: "取消",
                        color: MUTED,
                    },
                ];
                if let Some(index) = action_button_at(rows[2], &buttons, point) {
                    return Some(MouseAction::ActivateDialogButton(index));
                }
                if rows[1].contains(point) {
                    let start = discovery_view_offset(
                        filtered.len(),
                        rows[1].height as usize,
                        *cursor,
                        *current_scroll,
                    );
                    let index = start + (event.row - rows[1].y) as usize;
                    if index < filtered.len() {
                        return Some(MouseAction::SelectDiscovered {
                            index,
                            toggle: button == MouseButton::Left,
                        });
                    }
                }
            }
        }
        Overlay::KnownModelsLoading { .. } => {
            let area = modal_rect(terminal, 58, 9, 84, 54);
            if matches!(event.kind, MouseEventKind::Down(MouseButton::Right))
                || matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
                    && !area.contains(point)
            {
                return Some(MouseAction::Key(KeyCode::Esc, KeyModifiers::NONE));
            }
        }
        Overlay::KnownModelsPicker { .. } => {
            let area = modal_rect(terminal, 92, 30, 94, 90);
            if let Some(amount) = scroll
                && area.contains(point)
            {
                return Some(MouseAction::ScrollOverlay(amount));
            }
            if matches!(event.kind, MouseEventKind::Down(MouseButton::Left))
                && !area.contains(point)
            {
                return Some(MouseAction::Key(KeyCode::Esc, KeyModifiers::NONE));
            }
        }
    }
    None
}

fn apply_mouse_action(app: &mut App, action: MouseAction) {
    match action {
        MouseAction::Key(code, modifiers) => {
            app.handle_event(Event::Key(KeyEvent::new(code, modifiers)));
        }
        MouseAction::ActivateDialogButton(index) => {
            let activated = match app.overlay.as_mut() {
                Some(Overlay::Templates { focus, .. })
                | Some(Overlay::DiscoveryPicker { focus, .. }) => {
                    *focus = DialogFocus::Actions(index);
                    true
                }
                Some(Overlay::Confirm {
                    selected_button, ..
                }) => {
                    *selected_button = index;
                    true
                }
                _ => false,
            };
            if activated {
                app.handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )));
            }
        }
        MouseAction::FocusPane(pane) => app.mouse_focus_pane(pane),
        MouseAction::SelectList {
            pane,
            index,
            activate,
        } => app.mouse_select_list(pane, index, activate),
        MouseAction::ScrollList { pane, amount } => app.mouse_scroll_list(pane, amount),
        MouseAction::ActivateSearch => app.mouse_activate_search(),
        MouseAction::ClearSearch => app.mouse_clear_search(),
        MouseAction::SelectTemplate { index, activate } => {
            if let Some(Overlay::Templates {
                selected, focus, ..
            }) = app.overlay.as_mut()
            {
                *selected = index;
                *focus = if activate {
                    DialogFocus::Actions(0)
                } else {
                    DialogFocus::Content
                };
            }
            if activate {
                app.handle_event(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                )));
            }
        }
        MouseAction::SelectFormField {
            index,
            value_column,
            activate,
        } => {
            if let Some(Overlay::Form(form)) = app.overlay.as_mut() {
                form.mouse_select_field(index, value_column);
                if let Some(direction) = activate {
                    form.mouse_activate_field(direction);
                }
            }
        }
        MouseAction::SelectDiscovered { index, toggle } => {
            if let Some(Overlay::DiscoveryPicker {
                choices,
                filtered,
                cursor,
                scroll,
                focus,
                ..
            }) = app.overlay.as_mut()
            {
                *cursor = index;
                *focus = DialogFocus::Content;
                if index < *scroll {
                    *scroll = index;
                }
                if toggle
                    && let Some(choice) = filtered
                        .get(index)
                        .and_then(|source_index| choices.get_mut(*source_index))
                    && !choice.exists
                {
                    choice.selected = !choice.selected;
                }
            }
        }
        MouseAction::ScrollOverlay(amount) => scroll_overlay(app, amount),
    }
}

fn scroll_overlay(app: &mut App, amount: isize) {
    match app.overlay.as_mut() {
        Some(Overlay::Help { scroll }) | Some(Overlay::Diagnostics { scroll }) => {
            *scroll = move_index(*scroll, amount, usize::MAX);
        }
        Some(Overlay::Templates {
            selected, focus, ..
        }) => {
            *selected = move_index(*selected, amount.signum(), PROVIDER_TEMPLATES.len() - 1);
            *focus = DialogFocus::Content;
        }
        Some(Overlay::Form(form)) => {
            let selected = move_index(
                form.selected,
                amount.signum(),
                form.fields.len().saturating_sub(1),
            );
            form.mouse_select_field(selected, usize::MAX);
        }
        Some(Overlay::DiscoveryPicker {
            filtered,
            cursor,
            scroll,
            focus,
            ..
        }) => {
            *cursor = move_index(*cursor, amount, filtered.len().saturating_sub(1));
            *focus = DialogFocus::Content;
            if *cursor < *scroll {
                *scroll = *cursor;
            }
        }
        Some(Overlay::KnownModelsPicker {
            filtered,
            cursor,
            scroll,
            focus,
            ..
        }) => {
            *cursor = move_index(*cursor, amount, filtered.len().saturating_sub(1));
            *focus = DialogFocus::Content;
            if *cursor < *scroll {
                *scroll = *cursor;
            }
        }
        _ => {}
    }
}

fn command_at(app: &App, area: Rect, column: u16) -> Option<CommandSpec> {
    let mut x = area.x;
    for command in command_specs(area.width, app) {
        let width = command.label.width() as u16;
        if column >= x && column < x.saturating_add(width) {
            return Some(command);
        }
        x = x.saturating_add(width);
        if x >= area.right() {
            break;
        }
    }
    None
}

fn list_index_at(length: usize, area: Rect, selected: usize, row: u16) -> Option<usize> {
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    if row < inner.y || row >= inner.bottom() || inner.height == 0 {
        return None;
    }
    let offset = list_view_offset(length, inner.height as usize, selected);
    let index = offset + (row - inner.y) as usize;
    (index < length).then_some(index)
}

fn list_view_offset(length: usize, visible: usize, selected: usize) -> usize {
    if visible == 0 || length <= visible {
        0
    } else {
        selected
            .saturating_sub(visible - 1)
            .min(length.saturating_sub(visible))
    }
}

fn form_field_start(form: &FormState, visible: usize) -> usize {
    let mut start = form.scroll.min(form.fields.len().saturating_sub(1));
    if form.selected < start {
        start = form.selected;
    } else if form.selected >= start.saturating_add(visible) {
        start = form.selected + 1 - visible;
    }
    start
}

fn discovery_view_offset(
    length: usize,
    visible: usize,
    cursor: usize,
    current_scroll: usize,
) -> usize {
    let mut scroll = current_scroll.min(length.saturating_sub(1));
    if cursor < scroll {
        scroll = cursor;
    } else if cursor >= scroll.saturating_add(visible) {
        scroll = cursor + 1 - visible.max(1);
    }
    scroll
}

fn action_button_at(area: Rect, buttons: &[ActionButton], point: Position) -> Option<usize> {
    action_button_rects(area, buttons)
        .iter()
        .position(|rect| rect.contains(point))
}

fn move_index(current: usize, amount: isize, max: usize) -> usize {
    if amount.is_negative() {
        current.saturating_sub(amount.unsigned_abs())
    } else {
        current.saturating_add(amount as usize).min(max)
    }
}

fn draw_too_small(frame: &mut Frame<'_>, area: Rect) {
    let text = Text::from(vec![
        Line::from(Span::styled(
            "IPMT",
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        )),
        Line::from("终端空间不足"),
        Line::from(Span::styled("至少需要 42 x 12", Style::default().fg(MUTED))),
    ]);
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(TEXT).bg(BG))
            .alignment(Alignment::Center)
            .block(Block::default().padding(Padding::vertical(2))),
        area,
    );
}

fn draw_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let providers = app.doc.providers();
    let model_count: usize = providers.iter().map(|provider| provider.model_count).sum();
    let (errors, warnings) = app.diagnostic_counts();
    let dirty = if app.is_dirty() {
        "未保存"
    } else {
        "已同步"
    };
    let dirty_color = if app.is_dirty() { YELLOW } else { GREEN };

    let first = Line::from(vec![
        Span::styled(
            " IPMT ",
            Style::default()
                .fg(BG)
                .bg(CYAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  Pi 模型与提供商", Style::default().fg(TEXT)),
        Span::raw("  "),
        Span::styled(dirty, Style::default().fg(dirty_color)),
        Span::styled(
            format!("  {}/{}", providers.len(), model_count),
            Style::default().fg(MUTED),
        ),
        if errors > 0 {
            Span::styled(format!("  错误 {errors}"), Style::default().fg(RED))
        } else if warnings > 0 {
            Span::styled(format!("  警告 {warnings}"), Style::default().fg(YELLOW))
        } else {
            Span::styled("  校验通过", Style::default().fg(GREEN))
        },
    ]);
    let path = truncate_to_width(
        &app.doc.path().display().to_string(),
        area.width as usize - 3,
    );
    let second = Line::from(vec![
        Span::styled(" 文件  ", Style::default().fg(MUTED)),
        Span::styled(path, Style::default().fg(TEXT)),
    ]);
    let search_prefix = if app.search_active {
        "/ 搜索  "
    } else {
        " 筛选  "
    };
    let search_value = if app.search.is_empty() {
        if app.search_active { "" } else { "全部" }
    } else {
        app.search.as_str()
    };
    let third = Line::from(vec![
        Span::styled(
            search_prefix,
            Style::default().fg(if app.search_active { CYAN } else { MUTED }),
        ),
        Span::styled(search_value, Style::default().fg(TEXT)),
    ]);

    frame.render_widget(
        Paragraph::new(vec![first, second, third]).style(Style::default().bg(PANEL)),
        area,
    );
    if app.search_active && app.overlay.is_none() {
        let x = area
            .x
            .saturating_add(8)
            .saturating_add(app.search.width() as u16)
            .min(area.right().saturating_sub(1));
        frame.set_cursor_position(Position::new(x, area.y + 2));
    }
}

fn draw_workspace(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let regions = workspace_regions(area, app.focus);
    if let Some(providers) = regions.providers {
        draw_providers(frame, app, providers);
    }
    if let Some(models) = regions.models {
        draw_models(frame, app, models);
    }
    draw_details(frame, app, regions.details);
}

fn draw_providers(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let providers = app.visible_providers();
    let title = if app.search.is_empty() {
        format!(" 提供商  {} ", providers.len())
    } else {
        format!(
            " 提供商  {}/{} ",
            providers.len(),
            app.doc.providers().len()
        )
    };
    let block = panel_block(&title, app.focus == Pane::Providers);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if providers.is_empty() {
        draw_empty(frame, inner, "没有匹配的提供商");
        return;
    }

    let items = providers
        .iter()
        .map(|provider| {
            let auth = match &provider.summary.credential {
                CredentialHint::Missing => Span::styled("--", Style::default().fg(MUTED)),
                CredentialHint::Environment {
                    available: true, ..
                } => Span::styled("E+", Style::default().fg(GREEN)),
                CredentialHint::Environment {
                    available: false, ..
                } => Span::styled("E!", Style::default().fg(YELLOW)),
                CredentialHint::Command => Span::styled("CMD", Style::default().fg(BLUE)),
                CredentialHint::Literal => Span::styled("KEY", Style::default().fg(GREEN)),
            };
            let mut spans = vec![auth, Span::raw(" ")];
            spans.push(Span::styled(
                truncate_to_width(
                    &provider.summary.id,
                    inner.width.saturating_sub(10) as usize,
                ),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ));
            if provider.summary.has_overrides {
                spans.push(Span::styled(" *", Style::default().fg(MAGENTA)));
            }
            spans.push(Span::styled(
                format!("  {}", provider.summary.model_count),
                Style::default().fg(MUTED),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.provider_cursor));
    let list = List::new(items)
        .highlight_symbol("> ")
        .highlight_style(Style::default().fg(CYAN).bg(SURFACE));
    frame.render_stateful_widget(list, inner, &mut state);
    draw_scrollbar(frame, inner, providers.len(), app.provider_cursor);
}

fn draw_models(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let models = app.visible_models();
    let total = app
        .selected_provider()
        .map(|provider| provider.summary.model_count)
        .unwrap_or(0);
    let title = if app.search.is_empty() {
        format!(" 模型  {} ", models.len())
    } else {
        format!(" 模型  {}/{} ", models.len(), total)
    };
    let block = panel_block(&title, app.focus == Pane::Models);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.selected_provider().is_none() {
        draw_empty(frame, inner, "先选择提供商");
        return;
    }
    if models.is_empty() {
        draw_empty(frame, inner, "没有自定义模型");
        return;
    }

    let items = models
        .iter()
        .map(|model| {
            let reasoning = if model.summary.reasoning {
                Span::styled("R", Style::default().fg(MAGENTA))
            } else {
                Span::styled("-", Style::default().fg(MUTED))
            };
            let vision = if model.summary.vision {
                Span::styled("V", Style::default().fg(BLUE))
            } else {
                Span::styled("-", Style::default().fg(MUTED))
            };
            let id = truncate_to_width(&model.summary.id, inner.width.saturating_sub(9) as usize);
            ListItem::new(Line::from(vec![
                reasoning,
                vision,
                Span::raw("  "),
                Span::styled(id, Style::default().fg(TEXT)),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.model_cursor));
    let list = List::new(items)
        .highlight_symbol("> ")
        .highlight_style(Style::default().fg(CYAN).bg(SURFACE));
    frame.render_stateful_widget(list, inner, &mut state);
    draw_scrollbar(frame, inner, models.len(), app.model_cursor);
}

fn draw_details(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let show_model = app.focus == Pane::Models && app.selected_model().is_some();
    let title = if show_model {
        " 模型详情 "
    } else {
        " 提供商详情 "
    };
    let block = panel_block(title, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = if show_model {
        model_detail_lines(app)
    } else {
        provider_detail_lines(app)
    };
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(TEXT).bg(PANEL))
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn provider_detail_lines(app: &App) -> Vec<Line<'static>> {
    let Some(provider) = app.selected_provider() else {
        return vec![Line::styled("无选择", Style::default().fg(MUTED))];
    };
    let object = app
        .doc
        .provider_value(&provider.summary.id)
        .and_then(Value::as_object);
    let auth = match &provider.summary.credential {
        CredentialHint::Missing => "由 auth.json / 环境提供或未设置".to_string(),
        CredentialHint::Environment { name, available } => {
            format!("${name} ({})", if *available { "可用" } else { "未设置" })
        }
        CredentialHint::Command => "命令解析（未执行）".into(),
        CredentialHint::Literal => "已配置（内容已遮罩）".into(),
    };
    let headers = object
        .and_then(|object| object.get("headers"))
        .and_then(Value::as_object)
        .map_or(0, serde_json::Map::len);
    let compat = object
        .and_then(|object| object.get("compat"))
        .and_then(Value::as_object)
        .map_or(0, serde_json::Map::len);
    let unknown = object.map_or(0, |object| {
        object
            .keys()
            .filter(|key| {
                !matches!(
                    key.as_str(),
                    "baseUrl"
                        | "api"
                        | "apiKey"
                        | "oauth"
                        | "headers"
                        | "authHeader"
                        | "compat"
                        | "models"
                        | "modelOverrides"
                )
            })
            .count()
    });

    let mut lines = vec![
        heading(provider.summary.id),
        detail(
            "API",
            provider.summary.api.unwrap_or_else(|| "继承内置".into()),
        ),
        detail(
            "地址",
            provider
                .summary
                .base_url
                .unwrap_or_else(|| "内置默认".into()),
        ),
        detail("认证", auth),
        detail("模型", provider.summary.model_count.to_string()),
        detail("Headers", headers.to_string()),
        detail("Compat", compat.to_string()),
    ];
    if provider.summary.has_overrides {
        let count = object
            .and_then(|object| object.get("modelOverrides"))
            .and_then(Value::as_object)
            .map_or(0, serde_json::Map::len);
        lines.push(detail("覆盖", format!("{count} 个内置模型")));
    }
    if unknown > 0 {
        lines.push(detail("保留字段", unknown.to_string()));
    }
    lines
}

fn model_detail_lines(app: &App) -> Vec<Line<'static>> {
    let (Some(provider), Some(model)) = (app.selected_provider(), app.selected_model()) else {
        return vec![Line::styled("无选择", Style::default().fg(MUTED))];
    };
    let value = app
        .doc
        .model_value(&provider.summary.id, model.source_index);
    let object = value.and_then(Value::as_object);
    let effective_api = model
        .summary
        .api
        .clone()
        .or_else(|| provider.summary.api.clone())
        .unwrap_or_else(|| "未设置".into());
    let input = if model.summary.vision {
        "文本、图像"
    } else {
        "文本"
    };
    let context = model
        .summary
        .context_window
        .map(format_number)
        .unwrap_or_else(|| "128,000（默认）".into());
    let max_tokens = model
        .summary
        .max_tokens
        .map(format_number)
        .unwrap_or_else(|| "16,384（默认）".into());
    let thinking = object
        .and_then(|object| object.get("thinkingLevelMap"))
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect::<Vec<_>>().join(", "))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "标准映射".into());
    let cost = object
        .and_then(|object| object.get("cost"))
        .and_then(Value::as_object)
        .map(|cost| {
            format!(
                "in {} / out {} / read {} / write {}",
                number_or_zero(cost.get("input")),
                number_or_zero(cost.get("output")),
                number_or_zero(cost.get("cacheRead")),
                number_or_zero(cost.get("cacheWrite")),
            )
        })
        .unwrap_or_else(|| "全部为 0（默认）".into());
    let unknown = object.map_or(0, |object| {
        object
            .keys()
            .filter(|key| {
                !matches!(
                    key.as_str(),
                    "id" | "name"
                        | "api"
                        | "reasoning"
                        | "thinkingLevelMap"
                        | "input"
                        | "contextWindow"
                        | "maxTokens"
                        | "cost"
                        | "headers"
                        | "compat"
                )
            })
            .count()
    });

    let mut lines = vec![
        heading(model.summary.id),
        detail("提供商", provider.summary.id),
        detail(
            "名称",
            model.summary.name.unwrap_or_else(|| "与 ID 相同".into()),
        ),
        detail("API", effective_api),
        detail("扩展思考", yes_no(model.summary.reasoning)),
        detail("输入", input),
        detail("上下文", context),
        detail("最大输出", max_tokens),
        detail("思考级别", thinking),
        detail("价格 / M", cost),
    ];
    if unknown > 0 {
        lines.push(detail("保留字段", unknown.to_string()));
    }
    lines
}

fn draw_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let color = match app.status.kind {
        StatusKind::Info => BLUE,
        StatusKind::Success => GREEN,
        StatusKind::Warning => YELLOW,
        StatusKind::Error => RED,
    };
    let flag = if app.read_only {
        "READ ONLY"
    } else if app.is_dirty() {
        "MODIFIED"
    } else {
        "SAVED"
    };
    let flag_color = if app.read_only || app.is_dirty() {
        YELLOW
    } else {
        GREEN
    };
    let flag_width = flag.width() as u16 + 2;
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(flag_width)])
        .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ", Style::default().bg(color)),
            Span::raw(" "),
            Span::styled(
                truncate_to_width(
                    &app.status.text,
                    columns[0].width.saturating_sub(3) as usize,
                ),
                Style::default().fg(TEXT),
            ),
        ]))
        .style(Style::default().bg(SURFACE)),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(flag)
            .alignment(Alignment::Center)
            .style(Style::default().fg(flag_color).bg(SURFACE)),
        columns[1],
    );
}

#[derive(Debug, Clone, Copy)]
struct CommandSpec {
    label: &'static str,
    code: KeyCode,
    modifiers: KeyModifiers,
    color: Color,
}

fn command_specs(width: u16, app: &App) -> Vec<CommandSpec> {
    let normal = KeyModifiers::NONE;
    let mut commands = if width >= 100 {
        vec![
            CommandSpec {
                label: " n 新增 ",
                code: KeyCode::Char('n'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " e 编辑 ",
                code: KeyCode::Char('e'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " d 删除 ",
                code: KeyCode::Char('d'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " c 复制 ",
                code: KeyCode::Char('c'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " / 搜索 ",
                code: KeyCode::Char('/'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " f 发现 ",
                code: KeyCode::Char('f'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " i 已知模型 ",
                code: KeyCode::Char('i'),
                modifiers: normal,
                color: BLUE,
            },
            CommandSpec {
                label: " s 保存 ",
                code: KeyCode::Char('s'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " v 校验 ",
                code: KeyCode::Char('v'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " F1 帮助 ",
                code: KeyCode::F(1),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " q 退出 ",
                code: KeyCode::Char('q'),
                modifiers: normal,
                color: MUTED,
            },
        ]
    } else if width >= 66 {
        vec![
            CommandSpec {
                label: " n 新增 ",
                code: KeyCode::Char('n'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " e 编辑 ",
                code: KeyCode::Char('e'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " d 删除 ",
                code: KeyCode::Char('d'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " / 搜索 ",
                code: KeyCode::Char('/'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " f 发现 ",
                code: KeyCode::Char('f'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " s 保存 ",
                code: KeyCode::Char('s'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " F1 帮助 ",
                code: KeyCode::F(1),
                modifiers: normal,
                color: MUTED,
            },
        ]
    } else {
        vec![
            CommandSpec {
                label: " n 新增 ",
                code: KeyCode::Char('n'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " e 编辑 ",
                code: KeyCode::Char('e'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " / 搜索 ",
                code: KeyCode::Char('/'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " s 保存 ",
                code: KeyCode::Char('s'),
                modifiers: normal,
                color: MUTED,
            },
            CommandSpec {
                label: " F1 帮助 ",
                code: KeyCode::F(1),
                modifiers: normal,
                color: MUTED,
            },
        ]
    };
    if app.can_undo() {
        commands.push(CommandSpec {
            label: " Ctrl+Z ",
            code: KeyCode::Char('z'),
            modifiers: KeyModifiers::CONTROL,
            color: BLUE,
        });
    }
    if app.can_redo() {
        commands.push(CommandSpec {
            label: " Ctrl+Y ",
            code: KeyCode::Char('y'),
            modifiers: KeyModifiers::CONTROL,
            color: BLUE,
        });
    }
    commands
}

fn draw_commands(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let line = command_specs(area.width, app)
        .into_iter()
        .map(|command| Span::styled(command.label, Style::default().fg(command.color)))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Line::from(line)).style(Style::default().bg(PANEL)),
        area,
    );
}

fn draw_overlay(frame: &mut Frame<'_>, app: &App, overlay: &Overlay, terminal: Rect) {
    match overlay {
        Overlay::Help { scroll } => draw_help(frame, terminal, *scroll),
        Overlay::Diagnostics { scroll } => {
            draw_diagnostics(frame, terminal, &app.diagnostics(), *scroll)
        }
        Overlay::Templates { selected, focus } => {
            draw_templates(frame, terminal, *selected, *focus)
        }
        Overlay::Confirm {
            title,
            message,
            selected_button,
            ..
        } => draw_confirm(frame, terminal, title, message, *selected_button),
        Overlay::Form(form) => draw_form(frame, terminal, form),
        Overlay::DiscoveryLoading { provider_id } => draw_loading(frame, terminal, provider_id),
        Overlay::DiscoveryPicker {
            provider_id,
            choices,
            filtered,
            query,
            filter_active,
            cursor,
            scroll,
            focus,
        } => draw_discovery_picker(
            frame,
            terminal,
            provider_id,
            choices,
            filtered,
            query,
            *filter_active,
            *cursor,
            *scroll,
            *focus,
        ),
        Overlay::KnownModelsLoading { provider_id } => {
            draw_known_models_loading(frame, terminal, provider_id)
        }
        Overlay::KnownModelsPicker {
            provider_id,
            choices,
            filtered,
            query,
            cursor,
            scroll,
            focus,
        } => draw_known_models_picker(
            frame,
            terminal,
            provider_id,
            choices,
            filtered,
            query,
            *cursor,
            *scroll,
            *focus,
        ),
    }
}

fn draw_help(frame: &mut Frame<'_>, terminal: Rect, scroll: usize) {
    let area = modal_rect(terminal, 78, 30, 84, 84);
    let block = modal_block(" 操作 ");
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let lines = vec![
        help_heading("导航"),
        help_row("↑ / ↓  j / k", "移动选择"),
        help_row("← / →  Tab", "切换提供商与模型"),
        help_row("Home / End", "跳到首项或末项"),
        help_row("/", "模糊搜索 ID、名称、API 和地址"),
        Line::default(),
        help_heading("编辑"),
        help_row("n", "在当前栏新增"),
        help_row("p / m", "新增提供商 / 模型"),
        help_row("Enter / e", "编辑当前项"),
        help_row("d / Delete", "删除当前项"),
        help_row("c", "复制当前项并生成唯一 ID"),
        help_row("Ctrl+Z / Ctrl+Y", "撤销 / 重做"),
        Line::default(),
        help_heading("文件与目录"),
        help_row("s / Ctrl+S", "校验并原子保存"),
        help_row("r", "重新载入磁盘文件"),
        help_row("v", "查看全部校验结果"),
        help_row("f", "从当前提供商发现远程模型"),
        help_row("i（模型栏）", "搜索在线已知模型并按能力参数导入"),
        Line::default(),
        help_heading("弹窗焦点"),
        help_row("Tab / Shift+Tab", "直接切换内容区与按钮区"),
        help_row("↑ / ↓", "移动内容项；末项向下进入按钮区"),
        help_row("← / →", "切换枚举值或下方按钮"),
        help_row("Enter / Space", "激活当前开关、选项或按钮"),
        help_row("Space", "切换开关"),
        help_row("F3", "临时显示 / 隐藏密钥"),
        help_row("Ctrl+K", "清空当前字段"),
        help_row("Ctrl+S", "应用表单修改"),
        help_row("Esc", "取消并关闭"),
        Line::default(),
        help_heading("鼠标"),
        help_row("左键", "选择列表项、字段或底部操作"),
        help_row("双击", "编辑提供商或模型"),
        help_row("右键", "直接编辑列表项；枚举字段反向切换"),
        help_row("滚轮", "移动列表选择或滚动弹窗"),
        Line::default(),
        Line::from(Span::styled(
            "保存前会校验配置。已有文件默认创建时间戳备份；密钥不会出现在详情或错误输出中。",
            Style::default().fg(MUTED),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(TEXT).bg(PANEL))
            .scroll((scroll.min(u16::MAX as usize) as u16, 0))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn draw_diagnostics(
    frame: &mut Frame<'_>,
    terminal: Rect,
    diagnostics: &[Diagnostic],
    scroll: usize,
) {
    let area = modal_rect(terminal, 90, 28, 90, 82);
    let errors = diagnostics
        .iter()
        .filter(|item| item.severity == Severity::Error)
        .count();
    let warnings = diagnostics.len() - errors;
    let title = format!(" 校验  {errors} 错误 / {warnings} 警告 ");
    let block = modal_block(&title);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    if diagnostics.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "配置通过校验",
                    Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled("Esc 返回", Style::default().fg(MUTED))),
            ])
            .alignment(Alignment::Center)
            .block(Block::default().padding(Padding::vertical(2)))
            .style(Style::default().bg(PANEL)),
            inner,
        );
        return;
    }
    let lines = diagnostics
        .iter()
        .flat_map(|item| {
            let (mark, color) = match item.severity {
                Severity::Error => ("ERR", RED),
                Severity::Warning => ("WARN", YELLOW),
            };
            [
                Line::from(vec![
                    Span::styled(
                        format!(" {mark} "),
                        Style::default()
                            .fg(BG)
                            .bg(color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(item.path.clone(), Style::default().fg(CYAN)),
                ]),
                Line::from(vec![
                    Span::raw("      "),
                    Span::styled(item.message.clone(), Style::default().fg(TEXT)),
                ]),
            ]
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().bg(PANEL))
            .scroll((scroll.min(u16::MAX as usize) as u16, 0))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

#[derive(Debug, Clone, Copy)]
struct ActionButton {
    label: &'static str,
    color: Color,
}

fn action_button_rects(area: Rect, buttons: &[ActionButton]) -> Vec<Rect> {
    if buttons.is_empty() || area.is_empty() {
        return Vec::new();
    }
    let widths = buttons
        .iter()
        .map(|button| button.label.width() as u16 + 4)
        .collect::<Vec<_>>();
    let gaps = (buttons.len().saturating_sub(1) as u16) * 2;
    let total = widths.iter().sum::<u16>().saturating_add(gaps);
    if total <= area.width {
        let mut x = area.x + (area.width - total) / 2;
        return widths
            .into_iter()
            .map(|width| {
                let rect = Rect::new(x, area.y, width, 1);
                x = x.saturating_add(width).saturating_add(2);
                rect
            })
            .collect();
    }

    let width = area.width / buttons.len() as u16;
    (0..buttons.len())
        .map(|index| {
            let x = area.x + width * index as u16;
            let remaining = area.right().saturating_sub(x);
            Rect::new(
                x,
                area.y,
                if index + 1 == buttons.len() {
                    remaining
                } else {
                    width
                },
                1,
            )
        })
        .collect()
}

fn draw_action_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    buttons: &[ActionButton],
    focused: Option<usize>,
) {
    frame.render_widget(Block::default().style(Style::default().bg(PANEL)), area);
    for (index, (button, rect)) in buttons
        .iter()
        .zip(action_button_rects(area, buttons))
        .enumerate()
    {
        let style = if focused == Some(index) {
            Style::default()
                .fg(BG)
                .bg(button.color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(button.color).bg(SURFACE)
        };
        frame.render_widget(
            Paragraph::new(format!("[ {} ]", button.label))
                .alignment(Alignment::Center)
                .style(style),
            rect,
        );
    }
}

fn draw_templates(frame: &mut Frame<'_>, terminal: Rect, selected: usize, focus: DialogFocus) {
    let area = modal_rect(terminal, 72, 20, 82, 72);
    let block = modal_block(" 新增提供商 ");
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(1)])
        .split(inner);
    let items = PROVIDER_TEMPLATES
        .iter()
        .map(|template| {
            ListItem::new(vec![
                Line::from(Span::styled(
                    template.name,
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    template.description,
                    Style::default().fg(MUTED),
                )),
            ])
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(items).highlight_symbol("> ").highlight_style(
            Style::default()
                .fg(if focus == DialogFocus::Content {
                    CYAN
                } else {
                    MUTED
                })
                .bg(SURFACE),
        ),
        rows[0],
        &mut state,
    );
    draw_action_bar(
        frame,
        rows[1],
        &[
            ActionButton {
                label: "选择",
                color: GREEN,
            },
            ActionButton {
                label: "取消",
                color: MUTED,
            },
        ],
        match focus {
            DialogFocus::Content => None,
            DialogFocus::Actions(button) => Some(button),
        },
    );
}

fn draw_confirm(
    frame: &mut Frame<'_>,
    terminal: Rect,
    title: &str,
    message: &str,
    selected_button: usize,
) {
    let width = (message.width() as u16 + 8).clamp(44, 76);
    let area = modal_rect(terminal, width, 9, 90, 60);
    let block_title = format!(" {title} ");
    let block = modal_block(&block_title);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(message.to_owned())
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(Block::default().padding(Padding::vertical(1)))
            .style(Style::default().fg(TEXT).bg(PANEL)),
        rows[0],
    );
    draw_action_bar(
        frame,
        rows[1],
        &[
            ActionButton {
                label: "确认",
                color: RED,
            },
            ActionButton {
                label: "取消",
                color: MUTED,
            },
        ],
        Some(selected_button),
    );
}

fn draw_form(frame: &mut Frame<'_>, terminal: Rect, form: &FormState) {
    let preferred_height = (form.fields.len() as u16 + 7).clamp(15, 28);
    let area = modal_rect(terminal, 98, preferred_height, 94, 92);
    let block_title = format!(" {} ", form.title);
    let block = modal_block(&block_title);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(inner);
    let field_area = rows[0];
    let visible = field_area.height as usize;
    let start = form_field_start(form, visible);
    let end = (start + visible).min(form.fields.len());
    let label_width = if field_area.width >= 72 { 20 } else { 14 };

    for (row_offset, field) in form.fields[start..end].iter().enumerate() {
        let row = Rect::new(
            field_area.x,
            field_area.y + row_offset as u16,
            field_area.width,
            1,
        );
        let selected = form.fields_focused() && start + row_offset == form.selected;
        let row_style = if selected {
            Style::default().fg(TEXT).bg(SURFACE)
        } else {
            Style::default().fg(TEXT).bg(PANEL)
        };
        frame.render_widget(Block::default().style(row_style), row);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(label_width.min(row.width.saturating_sub(4))),
                Constraint::Min(2),
            ])
            .split(row);
        let label = truncate_to_width(field.label, columns[0].width.saturating_sub(2) as usize);
        frame.render_widget(
            Paragraph::new(format!(" {label}")).style(
                Style::default()
                    .fg(if selected { CYAN } else { MUTED })
                    .bg(row_style.bg.unwrap_or(PANEL)),
            ),
            columns[0],
        );
        let display = field.display_value(form.reveal_secrets);
        let placeholder = field.value.is_empty();
        let value_style = Style::default()
            .fg(if placeholder { MUTED } else { TEXT })
            .bg(row_style.bg.unwrap_or(PANEL));
        let cursor_width = if selected && field.is_editable_text() {
            form.cursor_display_width()
        } else {
            0
        };
        let available = columns[1].width.saturating_sub(1) as usize;
        let horizontal_scroll = cursor_width.saturating_sub(available.saturating_sub(1));
        let decoration = match &field.kind {
            FieldKind::Select(_) => "  < >",
            FieldKind::Bool => "  Space",
            FieldKind::Secret if !form.reveal_secrets => "  F3",
            _ => "",
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(display, value_style),
                Span::styled(
                    decoration,
                    Style::default().fg(MUTED).bg(row_style.bg.unwrap_or(PANEL)),
                ),
            ]))
            .style(row_style)
            .scroll((0, horizontal_scroll.min(u16::MAX as usize) as u16)),
            columns[1],
        );
        if selected && field.is_editable_text() {
            let x = columns[1]
                .x
                .saturating_add(cursor_width.saturating_sub(horizontal_scroll) as u16)
                .min(columns[1].right().saturating_sub(1));
            frame.set_cursor_position(Position::new(x, row.y));
        }
    }

    let hint_lines = if let Some(error) = &form.error {
        vec![
            Line::from(Span::styled(
                error.clone(),
                Style::default().fg(RED).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                form.current().hint,
                Style::default().fg(MUTED),
            )),
        ]
    } else if let Some(button) = form.focused_button() {
        let (message, hint, color) = if button == 0 {
            (
                "确定并应用修改",
                "左右键选择按钮，Tab 或向上返回字段区",
                GREEN,
            )
        } else {
            (
                "取消本次编辑",
                "左右键选择按钮，Tab 或向上返回字段区",
                YELLOW,
            )
        };
        vec![
            Line::from(Span::styled(
                message,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(hint, Style::default().fg(MUTED))),
        ]
    } else {
        vec![
            Line::from(Span::styled(
                form.current().hint,
                Style::default().fg(MUTED),
            )),
            Line::from(Span::styled(
                match form.current().kind {
                    FieldKind::Select(_) => "左右键切换选项",
                    FieldKind::Bool => "Space 或 Enter 切换",
                    FieldKind::JsonObject => "必须输入一个 JSON 对象",
                    FieldKind::Secret => "内容默认遮罩；F3 临时显示",
                    _ => "",
                },
                Style::default().fg(BLUE),
            )),
        ]
    };
    frame.render_widget(
        Paragraph::new(hint_lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(PANEL)),
        rows[1],
    );
    draw_action_bar(
        frame,
        rows[2],
        &[
            ActionButton {
                label: "确定",
                color: GREEN,
            },
            ActionButton {
                label: "取消",
                color: MUTED,
            },
        ],
        form.focused_button(),
    );
}

fn draw_loading(frame: &mut Frame<'_>, terminal: Rect, provider_id: &str) {
    let area = modal_rect(terminal, 54, 9, 84, 54);
    let block = modal_block(" 远程模型发现 ");
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        / 180;
    let spinner = ["|", "/", "-", "\\"][tick as usize % 4];
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(format!("{spinner}  "), Style::default().fg(CYAN)),
                Span::styled(provider_id.to_owned(), Style::default().fg(TEXT)),
            ]),
            Line::default(),
            Line::from(Span::styled(
                "正在读取模型目录...",
                Style::default().fg(MUTED),
            )),
            Line::from(Span::styled("Esc 忽略结果", Style::default().fg(MUTED))),
        ])
        .alignment(Alignment::Center)
        .block(Block::default().padding(Padding::vertical(1)))
        .style(Style::default().bg(PANEL)),
        inner,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_discovery_picker(
    frame: &mut Frame<'_>,
    terminal: Rect,
    provider_id: &str,
    choices: &[crate::app::DiscoveryChoice],
    filtered: &[usize],
    query: &str,
    filter_active: bool,
    cursor: usize,
    scroll: usize,
    focus: DialogFocus,
) {
    let area = modal_rect(terminal, 88, 28, 92, 88);
    let selected = choices.iter().filter(|item| item.selected).count();
    let existing = choices.iter().filter(|item| item.exists).count();
    let title = format!(
        " 远程模型  {provider_id}  /  {}/{}  选择 {selected}  已有 {existing} ",
        filtered.len(),
        choices.len()
    );
    let block = modal_block(&title);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(inner);
    let filter_value = if query.is_empty() {
        if filter_active {
            ""
        } else {
            "按 / 根据模型 ID 筛选"
        }
    } else {
        query
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                if filter_active {
                    "/ 筛选  "
                } else {
                    " 筛选  "
                },
                Style::default().fg(if filter_active { CYAN } else { MUTED }),
            ),
            Span::styled(filter_value, Style::default().fg(TEXT)),
        ]))
        .style(Style::default().bg(SURFACE)),
        rows[0],
    );
    let visible = rows[1].height as usize;
    let effective_scroll = discovery_view_offset(filtered.len(), visible, cursor, scroll);
    let end = (effective_scroll + visible).min(filtered.len());
    let items = filtered[effective_scroll..end]
        .iter()
        .filter_map(|index| choices.get(*index))
        .map(|choice| {
            let mark = if choice.exists {
                Span::styled("[=]", Style::default().fg(MUTED))
            } else if choice.selected {
                Span::styled("[x]", Style::default().fg(GREEN))
            } else {
                Span::styled("[ ]", Style::default().fg(MUTED))
            };
            let suffix = if choice.exists { "  已存在" } else { "" };
            ListItem::new(Line::from(vec![
                mark,
                Span::raw("  "),
                Span::styled(choice.model.id.clone(), Style::default().fg(TEXT)),
                Span::styled(suffix, Style::default().fg(MUTED)),
            ]))
        })
        .collect::<Vec<_>>();
    let local_cursor = cursor.saturating_sub(effective_scroll);
    let mut state = ListState::default().with_selected(Some(local_cursor));
    frame.render_stateful_widget(
        List::new(items).highlight_symbol("> ").highlight_style(
            Style::default()
                .fg(if focus == DialogFocus::Content {
                    CYAN
                } else {
                    MUTED
                })
                .bg(SURFACE),
        ),
        rows[1],
        &mut state,
    );
    draw_scrollbar(frame, rows[1], filtered.len(), cursor);
    draw_action_bar(
        frame,
        rows[2],
        &[
            ActionButton {
                label: "导入",
                color: GREEN,
            },
            ActionButton {
                label: "全选",
                color: TEXT,
            },
            ActionButton {
                label: "清空",
                color: YELLOW,
            },
            ActionButton {
                label: "取消",
                color: MUTED,
            },
        ],
        match focus {
            DialogFocus::Content => None,
            DialogFocus::Actions(button) => Some(button),
        },
    );
    if filter_active {
        let x = rows[0]
            .x
            .saturating_add(10)
            .saturating_add(query.width() as u16)
            .min(rows[0].right().saturating_sub(1));
        frame.set_cursor_position(Position::new(x, rows[0].y));
    }
}

fn draw_known_models_loading(frame: &mut Frame<'_>, terminal: Rect, provider_id: &str) {
    let area = modal_rect(terminal, 58, 9, 84, 54);
    let block = modal_block(" 已知模型目录 ");
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        / 180;
    let spinner = ["|", "/", "-", "\\"][tick as usize % 4];
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(format!("{spinner}  "), Style::default().fg(CYAN)),
                Span::styled(provider_id.to_owned(), Style::default().fg(TEXT)),
            ]),
            Line::default(),
            Line::styled(
                "正在合并 sub2api 与 router-for-me 最新模型数据...",
                Style::default().fg(MUTED),
            ),
            Line::styled("Esc 忽略结果", Style::default().fg(MUTED)),
        ])
        .alignment(Alignment::Center)
        .block(Block::default().padding(Padding::vertical(1)))
        .style(Style::default().bg(PANEL)),
        inner,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_known_models_picker(
    frame: &mut Frame<'_>,
    terminal: Rect,
    provider_id: &str,
    choices: &[crate::app::KnownModelChoice],
    filtered: &[usize],
    query: &str,
    cursor: usize,
    scroll: usize,
    focus: DialogFocus,
) {
    let area = modal_rect(terminal, 92, 30, 94, 90);
    let title = format!(
        " 已知模型快速导入  {provider_id}  /  {}/{} ",
        filtered.len(),
        choices.len()
    );
    let block = modal_block(&title);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("搜索模型 ID  ", Style::default().fg(CYAN)),
            Span::styled(
                if query.is_empty() {
                    "直接输入..."
                } else {
                    query
                },
                Style::default().fg(if query.is_empty() { MUTED } else { TEXT }),
            ),
        ]))
        .style(Style::default().bg(SURFACE)),
        rows[0],
    );
    let visible = rows[1].height as usize;
    let effective_scroll = discovery_view_offset(filtered.len(), visible, cursor, scroll);
    let end = (effective_scroll + visible).min(filtered.len());
    let items = filtered[effective_scroll..end]
        .iter()
        .filter_map(|index| choices.get(*index))
        .map(|choice| {
            let value = choice.model.value.as_object();
            let reasoning = value
                .and_then(|v| v.get("reasoning"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let vision = value
                .and_then(|v| v.get("input"))
                .and_then(Value::as_array)
                .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("image")));
            ListItem::new(Line::from(vec![
                Span::styled(
                    if choice.exists { "[=]" } else { "[+]" },
                    Style::default().fg(if choice.exists { MUTED } else { GREEN }),
                ),
                Span::styled(
                    format!(
                        " {}{}  ",
                        if reasoning { "R" } else { "-" },
                        if vision { "V" } else { "-" }
                    ),
                    Style::default().fg(MAGENTA),
                ),
                Span::styled(choice.model.id.clone(), Style::default().fg(TEXT)),
                Span::styled(
                    format!("  {}", choice.model.family),
                    Style::default().fg(MUTED),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default()
        .with_selected((!items.is_empty()).then_some(cursor.saturating_sub(effective_scroll)));
    frame.render_stateful_widget(
        List::new(items).highlight_symbol("> ").highlight_style(
            Style::default()
                .fg(if focus == DialogFocus::Content {
                    CYAN
                } else {
                    MUTED
                })
                .bg(SURFACE),
        ),
        rows[1],
        &mut state,
    );
    draw_scrollbar(frame, rows[1], filtered.len(), cursor);
    draw_action_bar(
        frame,
        rows[2],
        &[ActionButton {
            label: "导入",
            color: GREEN,
        }],
        match focus {
            DialogFocus::Content => None,
            DialogFocus::Actions(_) => Some(0),
        },
    );
    if focus == DialogFocus::Content {
        let x = rows[0]
            .x
            .saturating_add(15)
            .saturating_add(query.width() as u16)
            .min(rows[0].right().saturating_sub(1));
        frame.set_cursor_position(Position::new(x, rows[0].y));
    }
}

fn panel_block(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .title(Line::from(Span::styled(
            title,
            Style::default()
                .fg(if focused { CYAN } else { MUTED })
                .add_modifier(if focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        )))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(if focused { CYAN } else { BORDER }))
        .style(Style::default().fg(TEXT).bg(PANEL))
}

fn modal_block(title: &str) -> Block<'_> {
    Block::default()
        .title(Line::from(Span::styled(
            title,
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        )))
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(CYAN))
        .padding(Padding::horizontal(1))
        .style(Style::default().fg(TEXT).bg(PANEL))
}

fn draw_empty(frame: &mut Frame<'_>, area: Rect, message: &str) {
    frame.render_widget(
        Paragraph::new(message.to_owned())
            .alignment(Alignment::Center)
            .block(Block::default().padding(Padding::vertical(1)))
            .style(Style::default().fg(MUTED).bg(PANEL)),
        area,
    );
}

fn draw_scrollbar(frame: &mut Frame<'_>, area: Rect, length: usize, position: usize) {
    if length <= area.height as usize || area.height < 2 {
        return;
    }
    let mut state = ScrollbarState::new(length).position(position);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_style(Style::default().fg(BORDER))
            .track_style(Style::default().fg(SURFACE)),
        area.inner(Margin {
            horizontal: 0,
            vertical: 0,
        }),
        &mut state,
    );
}

fn modal_rect(
    terminal: Rect,
    preferred_width: u16,
    preferred_height: u16,
    max_width_percent: u16,
    max_height_percent: u16,
) -> Rect {
    let max_width = terminal
        .width
        .saturating_mul(max_width_percent)
        .saturating_div(100)
        .max(1);
    let max_height = terminal
        .height
        .saturating_mul(max_height_percent)
        .saturating_div(100)
        .max(1);
    let width = preferred_width.min(max_width).max(1);
    let height = preferred_height.min(max_height).max(1);
    Rect::new(
        terminal.x + terminal.width.saturating_sub(width) / 2,
        terminal.y + terminal.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn heading(value: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        value.into(),
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
    ))
}

fn detail(label: &'static str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<10}"), Style::default().fg(MUTED)),
        Span::styled(value.into(), Style::default().fg(TEXT)),
    ])
}

fn help_heading(value: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        value,
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
    ))
}

fn help_row(keys: &'static str, action: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{keys:<22}"), Style::default().fg(YELLOW)),
        Span::styled(action, Style::default().fg(TEXT)),
    ])
}

fn yes_no(value: bool) -> String {
    if value { "是" } else { "否" }.into()
}

fn number_or_zero(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_f64)
        .map(|number| {
            if number.fract() == 0.0 {
                format!("{number:.0}")
            } else {
                format!("{number}")
            }
        })
        .unwrap_or_else(|| "0".into())
}

fn format_number(number: u64) -> String {
    let raw = number.to_string();
    let mut output = String::with_capacity(raw.len() + raw.len() / 3);
    for (index, character) in raw.chars().enumerate() {
        if index > 0 && (raw.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(character);
    }
    output
}

fn truncate_to_width(value: &str, max_width: usize) -> String {
    if value.width() <= max_width {
        return value.to_owned();
    }
    if max_width <= 1 {
        return "…".chars().take(max_width).collect();
    }
    let mut output = String::new();
    let target = max_width - 1;
    for grapheme in value.graphemes(true) {
        if output.width() + grapheme.width() > target {
            break;
        }
        output.push_str(grapheme);
    }
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    use super::*;
    use crate::config::ConfigDocument;

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn mouse_app() -> App {
        App::new(
            ConfigDocument::from_value(
                "/tmp/models.json",
                json!({
                    "providers": {
                        "first": {
                            "baseUrl": "http://localhost:11434/v1",
                            "api": "openai-completions",
                            "models": [{"id": "one"}, {"id": "two"}]
                        },
                        "second": {
                            "baseUrl": "https://example.com/v1",
                            "api": "openai-completions",
                            "models": [{"id": "remote"}]
                        }
                    }
                }),
            ),
            false,
            true,
        )
    }

    #[test]
    fn truncation_respects_wide_characters() {
        assert_eq!(truncate_to_width("模型-editor", 6), "模型-…");
        assert!(truncate_to_width("abc", 3).width() <= 3);
    }

    #[test]
    fn number_formatting_adds_groups() {
        assert_eq!(format_number(128000), "128,000");
        assert_eq!(format_number(42), "42");
    }

    #[test]
    fn mouse_selects_lists_and_applies_form_buttons() {
        let terminal = Rect::new(0, 0, 128, 38);
        let regions = screen_regions(terminal);
        let workspace = workspace_regions(regions.workspace, Pane::Providers);
        let providers = workspace.providers.unwrap();
        let mut app = mouse_app();

        handle_mouse(
            &mut app,
            mouse(
                MouseEventKind::Down(MouseButton::Right),
                providers.x + 2,
                providers.y + 2,
            ),
            terminal,
        );
        assert_eq!(app.provider_cursor, 1);
        assert!(matches!(app.overlay, Some(Overlay::Form(_))));

        let (form_area, auth_row, value_x) = {
            let form = match app.overlay.as_ref().unwrap() {
                Overlay::Form(form) => form,
                _ => unreachable!(),
            };
            let preferred_height = (form.fields.len() as u16 + 7).clamp(15, 28);
            let area = modal_rect(terminal, 98, preferred_height, 94, 92);
            let inner = modal_block("").inner(area);
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(4),
                    Constraint::Length(3),
                    Constraint::Length(1),
                ])
                .split(inner);
            let label_width = if rows[0].width >= 72 { 20 } else { 14 };
            (area, rows[0].y + 5, rows[0].x + label_width)
        };
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), value_x, auth_row),
            terminal,
        );
        let form = match app.overlay.as_ref().unwrap() {
            Overlay::Form(form) => form,
            _ => unreachable!(),
        };
        assert!(form.fields[5].bool_value());

        let inner = modal_block("").inner(form_area);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(4),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(inner);
        let buttons = [
            ActionButton {
                label: "确定",
                color: GREEN,
            },
            ActionButton {
                label: "取消",
                color: MUTED,
            },
        ];
        let apply = action_button_rects(rows[2], &buttons)[0];
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), apply.x, apply.y),
            terminal,
        );
        assert!(app.overlay.is_none());
        assert_eq!(
            app.doc.provider_value("second").unwrap()["authHeader"],
            true
        );
    }

    #[test]
    fn mouse_clicks_search_and_footer_help() {
        let terminal = Rect::new(0, 0, 128, 38);
        let regions = screen_regions(terminal);
        let mut app = mouse_app();
        handle_mouse(
            &mut app,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                regions.header.x + 4,
                regions.header.y + 2,
            ),
            terminal,
        );
        assert!(app.search_active);

        app.search_active = false;
        let commands = command_specs(regions.commands.width, &app);
        let help_index = commands
            .iter()
            .position(|command| command.code == KeyCode::F(1))
            .unwrap();
        let x = regions.commands.x
            + commands[..help_index]
                .iter()
                .map(|command| command.label.width() as u16)
                .sum::<u16>();
        handle_mouse(
            &mut app,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                x,
                regions.commands.y,
            ),
            terminal,
        );
        assert!(matches!(app.overlay, Some(Overlay::Help { .. })));
    }

    #[test]
    fn clicking_empty_list_background_switches_panel_focus() {
        let terminal = Rect::new(0, 0, 128, 38);
        let regions = screen_regions(terminal);
        let workspace = workspace_regions(regions.workspace, Pane::Providers);
        let providers = workspace.providers.unwrap();
        let models = workspace.models.unwrap();
        let mut app = mouse_app();

        handle_mouse(
            &mut app,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                models.x + 3,
                models.y + 10,
            ),
            terminal,
        );
        assert_eq!(app.focus, Pane::Models);
        assert_eq!(app.model_cursor, 0);

        handle_mouse(
            &mut app,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                providers.x + 3,
                providers.y + 10,
            ),
            terminal,
        );
        assert_eq!(app.focus, Pane::Providers);
        assert_eq!(app.provider_cursor, 0);
    }

    #[test]
    fn rendered_details_never_contain_literal_credentials() {
        let doc = ConfigDocument::from_value(
            "/tmp/models.json",
            json!({
                "providers": {
                    "local": {
                        "baseUrl": "http://localhost:11434/v1",
                        "api": "openai-completions",
                        "apiKey": "super-secret-token",
                        "models": [{"id": "model-a", "reasoning": true}]
                    }
                }
            }),
        );
        let app = App::new(doc, false, true);
        for (width, height) in [(128, 38), (60, 24)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| draw(frame, &app)).unwrap();
            let rendered = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(rendered.contains("IPMT"));
            assert!(!rendered.contains("super-secret-token"));
        }
    }
}
