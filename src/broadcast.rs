use anyhow::Result;
use ansi_to_tui::IntoText;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame, Terminal,
};
use std::borrow::Cow;
use std::thread;
use std::time::{Duration, Instant};

use crate::herdr::{self, PaneInfo};
use crate::layout;

const READ_LINES: usize = 14;
const CHROME_ROWS: u16 = 5;
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const SEND_REFRESH_DELAY: Duration = Duration::from_millis(120);

#[derive(Clone, Copy)]
enum Payload<'a> {
    Text(&'a str),
    Key(&'a str),
}

struct Target {
    pane_id: String,
    label: String,
    dead: bool,
    mirror: Vec<Line<'static>>,
}

pub fn run(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    selected: Vec<PaneInfo>,
) -> Result<()> {
    let mut targets: Vec<Target> = selected
        .into_iter()
        .map(|p| Target {
            label: label_of(&p),
            pane_id: p.pane_id,
            dead: false,
            mirror: Vec::new(),
        })
        .collect();
    let mut input = String::new();
    let mut sent: usize = 0;
    let mut notice: Option<String> = None;
    let mut cursor_on = true;
    let mut next_poll = Instant::now();

    let master = std::env::var("SYNC_PANES_MASTER").ok();
    let probe = master
        .clone()
        .or_else(|| targets.first().map(|t| t.pane_id.clone()))
        .unwrap_or_default();
    let snapshot = layout::capture(&probe);

    loop {
        if Instant::now() >= next_poll {
            let read_lines = terminal
                .size()
                .map(|s| (s.height.saturating_sub(CHROME_ROWS)).max(1) as usize)
                .unwrap_or(READ_LINES);
            refresh(&mut targets, read_lines);
            cursor_on = !cursor_on;
            next_poll = Instant::now() + POLL_INTERVAL;
        }
        let timeout = next_poll.saturating_duration_since(Instant::now());
        terminal.draw(|f| draw(f, &targets, &input, sent, &notice, cursor_on))?;

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                notice = None;
                match (key.code, key.modifiers) {
                    (KeyCode::Char('q'), KeyModifiers::CONTROL) => break,
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        send_all(&mut targets, Payload::Key("ctrl+c"));
                        input.clear();
                        next_poll = Instant::now() + SEND_REFRESH_DELAY;
                    }
                    (KeyCode::Esc, _) => {
                        send_all(&mut targets, Payload::Key("esc"));
                        next_poll = Instant::now() + SEND_REFRESH_DELAY;
                    }
                    (KeyCode::Enter, _) => {
                        send_all(&mut targets, Payload::Key("enter"));
                        input.clear();
                        next_poll = Instant::now() + SEND_REFRESH_DELAY;
                    }
                    (KeyCode::Backspace, _) => {
                        send_all(&mut targets, Payload::Key("backspace"));
                        input.pop();
                        next_poll = Instant::now() + SEND_REFRESH_DELAY;
                    }
                    (KeyCode::Char(ch), KeyModifiers::NONE) | (KeyCode::Char(ch), KeyModifiers::SHIFT) => {
                        let mut buf = [0u8; 4];
                        send_all(&mut targets, Payload::Text(ch.encode_utf8(&mut buf)));
                        input.push(ch);
                        next_poll = Instant::now() + SEND_REFRESH_DELAY;
                    }
                    (KeyCode::Char(ch), KeyModifiers::CONTROL) => {
                        notice = Some(format!("ctrl+{} is outside the v1 key set — not sent", ch));
                    }
                    _ => {
                        notice = Some("key not in the v1 key set — not sent".to_string());
                    }
                }
                sent = sent.saturating_add(1);
            }
        }

        if !targets.is_empty() && targets.iter().all(|t| t.dead) {
            notice = Some("all targets are gone — exiting".to_string());
            terminal.draw(|f| draw(f, &targets, &input, sent, &notice, cursor_on))?;
            thread::sleep(Duration::from_millis(1200));
            break;
        }
    }

    let mut probes: Vec<&str> = Vec::new();
    if let Some(m) = master.as_deref() {
        probes.push(m);
    }
    for t in &targets {
        probes.push(t.pane_id.as_str());
    }
    layout::exit_restore(&snapshot, &probes);
    Ok(())
}

fn send_all(targets: &mut [Target], payload: Payload) {
    thread::scope(|s| {
        let mut handles = Vec::new();
        for t in targets.iter_mut().filter(|t| !t.dead) {
            let pane = t.pane_id.clone();
            handles.push((t, s.spawn(move || match payload {
                Payload::Text(text) => herdr::send_text(&pane, text),
                Payload::Key(key) => herdr::send_key(&pane, key),
            })));
        }
        for (t, handle) in handles {
            match handle.join() {
                Ok(Ok(())) => {}
                _ => t.dead = true,
            }
        }
    });
}

fn refresh(targets: &mut [Target], lines: usize) {
    thread::scope(|s| {
        let mut handles = Vec::new();
        for t in targets.iter_mut().filter(|t| !t.dead) {
            let pane = t.pane_id.clone();
            handles.push((t, s.spawn(move || herdr::read_visible(&pane, lines))));
        }
        for (t, handle) in handles {
            match handle.join() {
                Ok(Ok(out)) => t.mirror = parse_ansi(&out),
                _ => t.dead = true,
            }
        }
    });
}

fn parse_ansi(raw: &str) -> Vec<Line<'static>> {
    match raw.into_text() {
        Ok(text) => text
            .lines
            .into_iter()
            .map(|l| {
                let style = l.style;
                Line::from(
                    l.spans
                        .into_iter()
                        .map(|s| Span::styled(s.content.into_owned(), style.patch(s.style)))
                        .collect::<Vec<_>>(),
                )
            })
            .collect(),
        Err(_) => strip_ansi(raw)
            .lines()
            .map(|l| Line::from(l.to_string()))
            .collect(),
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            Some(']') => {
                let mut prev = '\0';
                for c2 in chars.by_ref() {
                    if c2 == '\x07' {
                        break;
                    }
                    if prev == '\x1b' && c2 == '\\' {
                        break;
                    }
                    prev = c2;
                }
            }
            _ => {}
        }
    }
    out
}

fn label_of(info: &PaneInfo) -> String {
    match &info.agent {
        Some(a) => format!("{} {}", info.pane_id, a),
        None => format!("{} shell", info.pane_id),
    }
}

fn draw(
    f: &mut Frame,
    targets: &[Target],
    input: &str,
    sent: usize,
    notice: &Option<String>,
    cursor_on: bool,
) {
    let area = f.area();
    let rows = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).split(area);

    let live = targets.iter().filter(|t| !t.dead).count();
    let mut status = format!(
        " live {}/{} · sent {} · enter ⏎ ⌫ esc ^c broadcast · ^q quit",
        live,
        targets.len(),
        sent
    );
    if let Some(n) = notice {
        status.push_str("  ·  ");
        status.push_str(n);
    }
    let header = Paragraph::new(vec![
        Line::from(format!(" > {input}▏")),
        Line::from(status).dim(),
    ])
    .block(Block::bordered().title(" sync-panes — broadcasting ".bold()));
    f.render_widget(header, rows[0]);

    let cells = Layout::horizontal(targets.iter().map(|_| Constraint::Fill(1))).split(rows[1]);
    for (t, cell) in targets.iter().zip(cells.iter()) {
        let (title, body) = if t.dead {
            (
                Line::from(format!(" {} ✕ dead ", t.label)).red(),
                Paragraph::new(Line::from(" pane closed or unreachable ").red()),
            )
        } else {
            let rows = cell.height.saturating_sub(2) as usize;
            let shown = &t.mirror[t.mirror.len().saturating_sub(rows)..];
            (
                Line::from(format!(" {} ● ", t.label)).green(),
                Paragraph::new(with_cursor(shown, cursor_on)),
            )
        };
        let block = Block::bordered().title(title);
        f.render_widget(body.block(block), *cell);
    }
}

fn with_cursor(lines: &[Line<'static>], cursor_on: bool) -> Vec<Line<'static>> {
    let mut out = lines.to_vec();
    if !cursor_on {
        if out.is_empty() {
            out.push(Line::from(""));
        }
        return out;
    }
    let row = out
        .iter()
        .rposition(|l| {
            l.spans
                .iter()
                .any(|s| s.content.contains(|c: char| !c.is_whitespace()))
        })
        .unwrap_or(out.len().saturating_sub(1));
    if let Some(l) = out.get_mut(row) {
        let mut spans = std::mem::take(&mut l.spans);
        while let Some(last) = spans.last() {
            if last.content.trim_end_matches(' ').is_empty() {
                spans.pop();
            } else {
                break;
            }
        }
        if let Some(last) = spans.last_mut() {
            let trimmed = last.content.trim_end().to_string();
            last.content = Cow::Owned(trimmed);
        }
        spans.push(Span::styled(
            " ",
            Style::default().add_modifier(Modifier::REVERSED),
        ));
        l.spans = spans;
    }
    out
}
