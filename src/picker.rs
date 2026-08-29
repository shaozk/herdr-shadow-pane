use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::Line,
    widgets::{Block, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use std::time::Duration;

use crate::herdr::{Context, PaneInfo};

struct PickItem {
    info: PaneInfo,
    selected: bool,
}

pub fn run(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    ctx: &Context,
    panes: Vec<PaneInfo>,
) -> Result<Option<Vec<PaneInfo>>> {
    let mut items: Vec<PickItem> = panes
        .into_iter()
        .map(|info| PickItem { selected: false, info })
        .collect();
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(0));
    }
    let mut notice: Option<String> = None;

    loop {
        terminal.draw(|f| draw(f, ctx, &items, &mut state, &notice))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let mut handled_notice = None;
            match (key.code, key.modifiers) {
                (KeyCode::Char('q'), KeyModifiers::CONTROL)
                | (KeyCode::Char('c'), KeyModifiers::CONTROL)
                | (KeyCode::Esc, _)
                | (KeyCode::Char('q'), _) => return Ok(None),
                (KeyCode::Up | KeyCode::Char('k'), _) => {
                    let next = state.selected().map(|s| s.saturating_sub(1)).unwrap_or(0);
                    state.select(Some(next));
                }
                (KeyCode::Down | KeyCode::Char('j'), _) => {
                    let next = state
                        .selected()
                        .map(|s| (s + 1).min(items.len().saturating_sub(1)))
                        .unwrap_or(0);
                    state.select(Some(next));
                }
                (KeyCode::Char(' '), _) => {
                    if let Some(i) = state.selected() {
                        if let Some(item) = items.get_mut(i) {
                            if !item.info.is_self {
                                item.selected = !item.selected;
                            }
                        }
                    }
                }
                (KeyCode::Char('a'), _) => {
                    let any_unselected = items.iter().any(|i| !i.info.is_self && !i.selected);
                    for item in items.iter_mut() {
                        if !item.info.is_self {
                            item.selected = any_unselected;
                        }
                    }
                }
                (KeyCode::Enter, _) => {
                    let selected: Vec<PaneInfo> = items
                        .iter()
                        .filter(|i| i.selected)
                        .map(|i| i.info.clone())
                        .collect();
                    if selected.is_empty() {
                        handled_notice = Some("select at least one pane (space) or press esc to quit".to_string());
                    } else {
                        return Ok(Some(selected));
                    }
                }
                _ => {}
            }
            notice = handled_notice;
        }
    }
}

fn draw(
    f: &mut Frame,
    ctx: &Context,
    items: &[PickItem],
    state: &mut ListState,
    notice: &Option<String>,
) {
    let area = f.area();
    let layout = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);

    let list_items: Vec<ListItem> = items
        .iter()
        .map(|item| {
            let info = &item.info;
            let checkbox = if info.is_self {
                "   "
            } else if item.selected {
                "[x]"
            } else {
                "[ ]"
            };
            let kind = match (&info.agent, &info.agent_status) {
                (Some(a), Some(s)) => format!("{a}({s})"),
                (Some(a), None) => a.clone(),
                _ => "shell".to_string(),
            };
            let desc = info
                .title
                .as_deref()
                .or(info.cwd.as_deref())
                .unwrap_or("");
            let mut line = Line::from(format!("{checkbox} {}  {kind}  {desc}", info.pane_id));
            if info.is_self {
                line = line.style(Style::default().fg(Color::DarkGray));
            } else if item.selected {
                line = line.style(Style::default().add_modifier(Modifier::BOLD));
            }
            ListItem::new(line)
        })
        .collect();

    let others = items.iter().filter(|i| !i.info.is_self).count();
    let title = format!(
        " sync-panes — select targets in tab {} ({other} other pane{s}) ",
        ctx.tab_id,
        other = others,
        s = if others == 1 { "" } else { "s" }
    );
    let list = List::new(list_items)
        .block(Block::bordered().title(title.bold()))
        .highlight_symbol("> ")
        .highlight_style(Style::default().bg(Color::Rgb(40, 40, 60)));
    f.render_stateful_widget(list, layout[0], state);

    let footer = match notice {
        Some(n) => Line::from(format!(" {n}")).style(Style::default().fg(Color::Yellow)),
        None => Line::from(" ↑/↓ move · space select · a all · enter broadcast · esc quit").dim(),
    };
    f.render_widget(Paragraph::new(footer), layout[1]);
}
