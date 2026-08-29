use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Stylize},
    text::Line,
    widgets::{Block, Paragraph},
    Frame, Terminal,
};
use std::thread;
use std::time::{Duration, Instant};

use crate::herdr::{self, PaneInfo};

const READ_LINES: usize = 14;
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
    output: String,
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
            output: String::new(),
        })
        .collect();
    let mut input = String::new();
    let mut sent: usize = 0;
    let mut notice: Option<String> = None;
    let mut next_poll = Instant::now();

    loop {
        if Instant::now() >= next_poll {
            refresh(&mut targets);
            next_poll = Instant::now() + POLL_INTERVAL;
        }
        let timeout = next_poll.saturating_duration_since(Instant::now());
        terminal.draw(|f| draw(f, &targets, &input, sent, &notice))?;

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
            terminal.draw(|f| draw(f, &targets, &input, sent, &notice))?;
            thread::sleep(Duration::from_millis(1200));
            break;
        }
    }
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

fn refresh(targets: &mut [Target]) {
    thread::scope(|s| {
        let mut handles = Vec::new();
        for t in targets.iter_mut().filter(|t| !t.dead) {
            let pane = t.pane_id.clone();
            handles.push((t, s.spawn(move || herdr::read_visible(&pane, READ_LINES))));
        }
        for (t, handle) in handles {
            match handle.join() {
                Ok(Ok(out)) => t.output = out,
                _ => t.dead = true,
            }
        }
    });
}

fn label_of(info: &PaneInfo) -> String {
    match &info.agent {
        Some(a) => format!("{} {}", info.pane_id, a),
        None => format!("{} shell", info.pane_id),
    }
}

fn draw(f: &mut Frame, targets: &[Target], input: &str, sent: usize, notice: &Option<String>) {
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
            let lines: Vec<Line> = t.output.lines().map(Line::from).collect();
            (
                Line::from(format!(" {} ● ", t.label)).green(),
                Paragraph::new(lines),
            )
        };
        let block = Block::bordered().title(title);
        f.render_widget(body.block(block), *cell);
    }
}
