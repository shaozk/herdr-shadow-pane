use anyhow::Result;
use ansi_to_tui::IntoText;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::Paragraph,
    Frame, Terminal,
};
use std::borrow::Cow;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::herdr::{self, PaneInfo};
use crate::layout;

const READ_LINES: usize = 999;
const READ_TICK: Duration = Duration::from_millis(50);
const RENDER_TICK: Duration = Duration::from_millis(30);
const BLINK_TICKS: u32 = 16;
const INPUT_BATCH_WINDOW: Duration = Duration::from_millis(10);

enum InputMsg {
    Text(String),
    Key(&'static str),
}

enum MirrorMsg {
    Mirror(usize, Vec<Line<'static>>),
    Dead(usize),
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
    let mut sent: usize = 0;
    let mut notice: Option<String> = None;
    let mut cursor_on = true;
    let mut blink = 0u32;

    let master = std::env::var("SHADOW_PANE_MASTER").ok();
    let probe = master
        .clone()
        .or_else(|| targets.first().map(|t| t.pane_id.clone()))
        .unwrap_or_default();
    let snapshot = layout::capture(&probe);

    let (mirror_tx, mirror_rx) = mpsc::channel::<MirrorMsg>();
    let mut workers = Vec::new();
    for (idx, t) in targets.iter().enumerate() {
        let tx = mirror_tx.clone();
        let pane = t.pane_id.clone();
        workers.push(thread::spawn(move || loop {
            let msg = match herdr::read_visible(&pane, READ_LINES) {
                Ok(out) => MirrorMsg::Mirror(idx, parse_ansi(&out)),
                Err(_) => MirrorMsg::Dead(idx),
            };
            if tx.send(msg).is_err() {
                return;
            }
            thread::sleep(READ_TICK);
        }));
    }
    drop(mirror_tx);

    let (input_tx, input_rx) = mpsc::channel::<InputMsg>();
    let send_panes: Vec<String> = targets.iter().map(|t| t.pane_id.clone()).collect();
    let sender = thread::spawn(move || {
        let mut dead: Vec<bool> = vec![false; send_panes.len()];
        while let Ok(first) = input_rx.recv() {
            let mut text = String::new();
            let mut keys: Vec<&'static str> = Vec::new();
            match first {
                InputMsg::Text(s) => text.push_str(&s),
                InputMsg::Key(k) => keys.push(k),
            }
            if text.is_empty() && keys.is_empty() {
                continue;
            }
            while let Ok(next) = input_rx.recv_timeout(INPUT_BATCH_WINDOW) {
                match next {
                    InputMsg::Text(s) => {
                        if !keys.is_empty() {
                            break;
                        }
                        text.push_str(&s);
                    }
                    InputMsg::Key(k) => {
                        keys.push(k);
                        break;
                    }
                }
            }
            if !text.is_empty() {
                dispatch(&send_panes, &mut dead, |p| herdr::send_text(p, &text));
            }
            for k in keys {
                dispatch(&send_panes, &mut dead, |p| herdr::send_key(p, k));
            }
        }
    });

    loop {
        while let Ok(msg) = mirror_rx.try_recv() {
            match msg {
                MirrorMsg::Mirror(idx, lines) => {
                    if let Some(t) = targets.get_mut(idx) {
                        t.mirror = lines;
                    }
                }
                MirrorMsg::Dead(idx) => {
                    if let Some(t) = targets.get_mut(idx) {
                        t.dead = true;
                    }
                }
            }
        }
        terminal.draw(|f| draw(f, &targets, sent, &notice, cursor_on))?;

        if event::poll(RENDER_TICK)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                notice = None;
                match (key.code, key.modifiers) {
                    (KeyCode::Char('q'), KeyModifiers::CONTROL) => break,
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        let _ = input_tx.send(InputMsg::Key("ctrl+c"));
                    }
                    (KeyCode::Esc, _) => {
                        let _ = input_tx.send(InputMsg::Key("esc"));
                    }
                    (KeyCode::Enter, _) => {
                        let _ = input_tx.send(InputMsg::Key("enter"));
                    }
                    (KeyCode::Backspace, _) => {
                        let _ = input_tx.send(InputMsg::Key("backspace"));
                    }
                    (KeyCode::Char(ch), KeyModifiers::NONE) | (KeyCode::Char(ch), KeyModifiers::SHIFT) => {
                        let mut buf = [0u8; 4];
                        let _ = input_tx.send(InputMsg::Text(ch.encode_utf8(&mut buf).to_string()));
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
        } else {
            blink = blink.saturating_add(1);
            if blink % BLINK_TICKS == 0 {
                cursor_on = !cursor_on;
            }
        }

        if !targets.is_empty() && targets.iter().all(|t| t.dead) {
            notice = Some("all targets are gone — exiting".to_string());
            terminal.draw(|f| draw(f, &targets, sent, &notice, cursor_on))?;
            thread::sleep(Duration::from_millis(1200));
            break;
        }
    }

    drop(input_tx);
    drop(mirror_rx);
    for w in workers {
        let _ = w.join();
    }
    let _ = sender.join();

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

fn dispatch<F>(panes: &[String], dead: &mut [bool], send: F)
where
    F: Fn(&str) -> anyhow::Result<()> + Sync,
{
    thread::scope(|s| {
        let mut handles = Vec::new();
        for (i, pane) in panes.iter().enumerate() {
            if dead[i] {
                continue;
            }
            let send = &send;
            handles.push((i, s.spawn(move || send(pane))));
        }
        for (i, handle) in handles {
            if handle.join().map(|r| r.is_err()).unwrap_or(true) {
                dead[i] = true;
            }
        }
    });
}

fn parse_ansi(raw: &str) -> Vec<Line<'static>> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "");
    match normalized.as_str().into_text() {
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
        Err(_) => strip_ansi(&normalized)
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
    sent: usize,
    notice: &Option<String>,
    cursor_on: bool,
) {
    let area = f.area();
    let constraints: Vec<Constraint> = match notice {
        Some(_) => vec![Constraint::Length(1), Constraint::Min(1)],
        None => vec![Constraint::Min(1)],
    };
    let rows = Layout::vertical(constraints).split(area);
    let body = if notice.is_some() { rows[1] } else { rows[0] };

    if let Some(n) = notice {
        let live = targets.iter().filter(|t| !t.dead).count();
        let status = format!(
            " live {live}/{} · sent {sent} · {n} · ^q quit",
            targets.len()
        );
        f.render_widget(Paragraph::new(Line::from(status).dim()), rows[0]);
    }

    let mut horizontal: Vec<Constraint> = Vec::new();
    for i in 0..targets.len() {
        if i > 0 {
            horizontal.push(Constraint::Length(1));
        }
        horizontal.push(Constraint::Fill(1));
    }
    let cells = Layout::horizontal(horizontal).split(body);
    for (i, t) in targets.iter().enumerate() {
        let cell = cells[i * 2];
        let body = if t.dead {
            Paragraph::new(Line::from(format!(" ✕ {} dead — pane closed or unreachable ", t.label)).red())
        } else {
            let rows = cell.height as usize;
            let shown = &t.mirror[t.mirror.len().saturating_sub(rows)..];
            Paragraph::new(with_cursor(shown, cursor_on, cell.width))
        };
        f.render_widget(body, cell);
        if let Some(sep) = cells.get(i * 2 + 1) {
            let line = vec![Line::from("│"); sep.height as usize];
            f.render_widget(Paragraph::new(line).dim(), *sep);
        }
    }
}

fn with_cursor(lines: &[Line<'static>], cursor_on: bool, width: u16) -> Vec<Line<'static>> {
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
        let line_width: u16 = spans.iter().map(|s| s.width() as u16).sum();
        l.spans = spans;
        let block =
            || Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED));
        if line_width < width {
            l.spans.push(block());
        } else if let Some(next) = out.get_mut(row + 1) {
            next.spans.insert(0, block());
        } else {
            out.push(Line::from(vec![block()]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect::<String>())
            .collect()
    }

    #[test]
    fn parse_ansi_keeps_lines_separate_under_cr() {
        let raw = "one\r\ntwo\r\nthree\r\n";
        assert_eq!(texts(&parse_ansi(raw)), vec!["one", "two", "three"]);
    }

    #[test]
    fn parse_ansi_real_stream_shape() {
        let raw = "seq 1 40\r\nhost# seq 1 40\r\n1\r\n2\r\n3\r\nhost# ";
        assert_eq!(
            texts(&parse_ansi(raw)),
            vec!["seq 1 40", "host# seq 1 40", "1", "2", "3", "host# "]
        );
    }

    #[test]
    fn shrinking_mirror_clears_old_content() {
        use ratatui::{backend::TestBackend, Terminal as TestTerminal};
        let backend = TestBackend::new(60, 10);
        let mut terminal = TestTerminal::new(backend).unwrap();
        let full = parse_ansi(&format!("{}\r\n", (1..=8).map(|i| format!("OLDLINE-{i}")).collect::<Vec<_>>().join("\r\n")));
        let mut targets = vec![Target {
            pane_id: "t".into(),
            label: "t".into(),
            dead: false,
            mirror: full,
        }];
        terminal
            .draw(|f| draw(f, &targets, 3, &None, true))
            .unwrap();
        targets[0].mirror = parse_ansi("host# ");
        terminal
            .draw(|f| draw(f, &targets, 4, &None, true))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(!screen.contains("OLDLINE"), "stale mirror content: {screen:?}");
    }

    #[test]
    fn cursor_wraps_when_anchor_line_fills_cell() {
        let width = 40u16;
        let mirror = parse_ansi(&format!("{}\r\n{}\r\n", "x".repeat(width as usize), " ".repeat(20)));
        let mirror = with_cursor(&mirror, true, width);
        let first: String = mirror[0].spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(first.chars().count(), width as usize);
        assert_eq!(mirror[1].spans[0].content, " ");
    }

    #[test]
    fn cursor_visible_in_helix_like_screen() {
        use ratatui::{backend::TestBackend, Terminal as TestTerminal};
        let backend = TestBackend::new(40, 6);
        let mut terminal = TestTerminal::new(backend).unwrap();
        let raw = format!("~\r\n~\r\n{}\r\n{}\r\n",
            format!("~   {}  1 sel  1:1 ", "~".repeat(3)),
            " ".repeat(40),
        );
        let targets = vec![Target {
            pane_id: "t".into(),
            label: "t".into(),
            dead: false,
            mirror: parse_ansi(&raw),
        }];
        terminal
            .draw(|f| draw(f, &targets, 0, &None, true))
            .unwrap();
        let mut reversed = 0usize;
        for cell in terminal.backend().buffer().content() {
            if cell.modifier.contains(Modifier::REVERSED) {
                reversed += 1;
            }
        }
        assert!(reversed >= 1, "shadow cursor must be visible, got {reversed} reversed cells");
    }
}
