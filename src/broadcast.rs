use anyhow::Result;
use ansi_to_tui::IntoText;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
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
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::herdr::{self, PaneInfo};
use crate::layout;

const READ_LINES: usize = 999;
const READ_TICK: Duration = Duration::from_millis(50);
const RENDER_TICK: Duration = Duration::from_millis(30);
const BLINK_TICKS: u32 = 16;
const WIDTH_REFRESH_TICK: Duration = Duration::from_millis(2000);
const INPUT_BATCH_WINDOW: Duration = Duration::from_millis(10);
const ANCHOR_MAX_CHANGE: usize = 16;
const ANCHOR_CHANGE_RATIO: usize = 3; // 1/3 of the wider line
const ANCHOR_SHIFT_MAX: i64 = 8;

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
    anchor: Option<(usize, u16)>,
    plain: Vec<String>,
    ver: u64,
    render: RenderCache,
}

#[derive(Default)]
struct RenderCache {
    lines: Vec<Line<'static>>,
    key: (u64, usize, Option<(usize, u16)>, bool, u16, usize),
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
            anchor: None,
            plain: Vec::new(),
            ver: 1,
            render: RenderCache::default(),
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

    let widths = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        String,
        u16,
    >::new()));
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    {
        let widths = widths.clone();
        let running = running.clone();
        let probe = probe.clone();
        thread::spawn(move || {
            while running.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(l) = herdr::tab_layout(&probe) {
                    let mut m = widths.lock().unwrap();
                    for (id, rect) in &l.panes {
                        m.insert(id.clone(), rect.width.max(1) as u16);
                    }
                }
                thread::sleep(WIDTH_REFRESH_TICK);
            }
        });
    }

    let (mirror_tx, mirror_rx) = mpsc::channel::<MirrorMsg>();
    let mut workers = Vec::new();
    for (idx, t) in targets.iter().enumerate() {
        let tx = mirror_tx.clone();
        let pane = t.pane_id.clone();
        let widths = widths.clone();
        workers.push(thread::spawn(move || {
            let mut socket = crate::herdr_socket::SocketClient::connect().ok();
            loop {
                let read = |socket: &mut Option<crate::herdr_socket::SocketClient>| {
                    match socket
                        .as_mut()
                        .map(|c| c.read_visible(&pane, READ_LINES))
                    {
                        Some(Ok(out)) => Ok(out),
                        Some(Err(_)) => {
                            // Socket degraded (server restart, protocol gap):
                            // fall back to the CLI path for the rest of the run.
                            *socket = None;
                            herdr::read_visible(&pane, READ_LINES)
                        }
                        None => herdr::read_visible(&pane, READ_LINES),
                    }
                };
                let msg = match read(&mut socket) {
                    Ok(out) => {
                        let width = widths
                            .lock()
                            .unwrap()
                            .get(&pane)
                            .copied()
                            .unwrap_or_default();
                        MirrorMsg::Mirror(idx, wrap_lines(parse_ansi(&out), width))
                    }
                    Err(_) => MirrorMsg::Dead(idx),
                };
                if tx.send(msg).is_err() {
                    return;
                }
                thread::sleep(READ_TICK);
            }
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
                        let new_plain: Vec<String> = lines.iter().map(plain_of).collect();
                        t.anchor = track_anchor_plain(&t.plain, &new_plain, t.anchor);
                        t.mirror = lines;
                        t.plain = new_plain;
                        t.ver = t.ver.wrapping_add(1);
                    }
                }
                MirrorMsg::Dead(idx) => {
                    if let Some(t) = targets.get_mut(idx) {
                        t.dead = true;
                    }
                }
            }
        }
        terminal.draw(|f| draw(f, &mut targets, sent, &notice, cursor_on))?;

        if event::poll(RENDER_TICK)? {
            match event::read()? {
                Event::Key(key) => {
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
                Event::Mouse(mouse) => {
                    if let Some(key) = wheel_key(mouse.kind) {
                        notice = None;
                        let _ = input_tx.send(InputMsg::Key(key));
                        sent = sent.saturating_add(1);
                    }
                }
                _ => {}
            }
        } else {
            blink = blink.saturating_add(1);
            if blink.is_multiple_of(BLINK_TICKS) {
                cursor_on = !cursor_on;
            }
        }

        if !targets.is_empty() && targets.iter().all(|t| t.dead) {
            notice = Some("all targets are gone — exiting".to_string());
            terminal.draw(|f| draw(f, &mut targets, sent, &notice, cursor_on))?;
            thread::sleep(Duration::from_millis(1200));
            break;
        }
    }

    drop(input_tx);
    drop(mirror_rx);
    running.store(false, std::sync::atomic::Ordering::Relaxed);
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

fn wheel_key(kind: MouseEventKind) -> Option<&'static str> {
    match kind {
        MouseEventKind::ScrollUp => Some("up"),
        MouseEventKind::ScrollDown => Some("down"),
        MouseEventKind::ScrollLeft => Some("left"),
        MouseEventKind::ScrollRight => Some("right"),
        _ => None,
    }
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
    targets: &mut [Target],
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
    for (i, t) in targets.iter_mut().enumerate() {
        let cell = cells[i * 2];
        let body = if t.dead {
            Paragraph::new(Line::from(format!(" ✕ {} dead — pane closed or unreachable ", t.label)).red())
        } else {
            let shown_rows = cell.height as usize;
            let cut = t.mirror.len().saturating_sub(shown_rows);
            let key = (t.ver, t.mirror.len(), t.anchor, cursor_on, cell.width, shown_rows);
            if t.render.key != key {
                let shown = &t.mirror[cut..];
                let anchor = t
                    .anchor
                    .map(|(r, c)| (r.saturating_sub(cut), c))
                    .filter(|(r, _)| *r < shown.len())
                    .or_else(|| {
                        let plain_cut = cut.min(t.plain.len());
                        heuristic_anchor(&t.plain[plain_cut..])
                    });
                t.render.lines = with_cursor(shown, anchor, cursor_on, cell.width);
                t.render.key = key;
            }
            Paragraph::new(t.render.lines.clone())
        };
        f.render_widget(body, cell);
        if let Some(sep) = cells.get(i * 2 + 1) {
            let line = vec![Line::from("│"); sep.height as usize];
            f.render_widget(Paragraph::new(line).dim(), *sep);
        }
    }
}

fn with_cursor(
    lines: &[Line<'static>],
    anchor: Option<(usize, u16)>,
    cursor_on: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let mut out = lines.to_vec();
    if !cursor_on {
        if out.is_empty() {
            out.push(Line::from(""));
        }
        return out;
    }
    if out.is_empty() {
        return out;
    }
    let Some((row, col)) = anchor.filter(|(r, _)| *r < out.len()) else {
        return out;
    };
    place_block(&mut out, row, col, width);
    out
}

const STATUS_MODE_TOKENS: &[&str] = &[
    "nor", "ins", "sel", "nop", "op", "normal", "insert", "visual", "select", "replace",
    "v-block", "v-line", "vblock", "vline",
];

fn looks_like_status_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with("--") && trimmed.ends_with("--") {
        return true;
    }
    trimmed
        .split(|c: char| c.is_whitespace() || c == '│' || c == '|')
        .any(|tok| STATUS_MODE_TOKENS.contains(&tok.to_lowercase().as_str()))
}

fn heuristic_anchor(plain: &[String]) -> Option<(usize, u16)> {
    let last = plain
        .iter()
        .rposition(|l| l.chars().any(|c| !c.is_whitespace()))?;
    if looks_like_status_line(&plain[last])
        && let Some(row) = plain[..last].iter().rposition(|l| {
            !l.chars().all(|c| c.is_whitespace()) && !looks_like_status_line(l)
        })
    {
        return Some((row, plain[row].trim_end().width() as u16));
    }
    Some((last, plain[last].trim_end().width() as u16))
}

fn block_span() -> Span<'static> {
    Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED))
}

fn place_block(out: &mut Vec<Line<'static>>, row: usize, col: u16, width: u16) {
    if col >= width {
        if let Some(next) = out.get_mut(row + 1) {
            insert_block_span(next, 0);
        } else {
            out.push(Line::from(vec![block_span()]));
        }
        return;
    }
    let line = &mut out[row];
    let full: u16 = line.spans.iter().map(|s| s.width() as u16).sum();
    if col >= full {
        trim_trailing_ws(line);
        line.spans.push(block_span());
        return;
    }
    insert_block_span(line, col);
}

fn trim_trailing_ws(line: &mut Line<'static>) {
    while line
        .spans
        .last()
        .is_some_and(|s| s.content.trim_end_matches(' ').is_empty())
    {
        line.spans.pop();
    }
    if let Some(last) = line.spans.last_mut() {
        last.content = Cow::Owned(last.content.trim_end().to_string());
    }
    if line.spans.is_empty() {
        line.spans.push(Span::from(""));
    }
}

fn split_at_width(s: &str, off: u16) -> (String, String) {
    let mut used: u16 = 0;
    for (bi, ch) in s.char_indices() {
        if used >= off {
            return (s[..bi].to_string(), s[bi..].to_string());
        }
        used += ch.width().unwrap_or(0) as u16;
    }
    (s.to_string(), String::new())
}

fn insert_block_span(line: &mut Line<'static>, col: u16) {
    let spans = std::mem::take(&mut line.spans);
    let mut new: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 2);
    let mut acc: u16 = 0;
    let mut placed = false;
    for span in spans {
        let w = span.width() as u16;
        if !placed && col < acc + w.max(1) {
            let (prefix, rest) = split_at_width(span.content.as_ref(), col.saturating_sub(acc));
            if !prefix.is_empty() {
                new.push(Span::styled(prefix, span.style));
            }
            let cover: String = rest
                .chars()
                .next()
                .map(|c| c.to_string())
                .unwrap_or_else(|| " ".to_string());
            new.push(Span::styled(cover.clone(), span.style.add_modifier(Modifier::REVERSED)));
            let after = &rest[cover.len()..];
            if !after.is_empty() {
                new.push(Span::styled(after.to_string(), span.style));
            }
            placed = true;
        } else {
            new.push(span);
        }
        acc = acc.saturating_add(w);
    }
    if !placed {
        new.push(block_span());
    }
    line.spans = new;
}

fn split_at_width_exact(s: &str, off: u16) -> (String, String) {
    let mut used: u16 = 0;
    for (bi, ch) in s.char_indices() {
        let w = ch.width().unwrap_or(0) as u16;
        if used + w > off {
            return (s[..bi].to_string(), s[bi..].to_string());
        }
        used += w;
        if used == off {
            let next = bi + ch.len_utf8();
            return (s[..next].to_string(), s[next..].to_string());
        }
    }
    (s.to_string(), String::new())
}

fn wrap_lines(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return lines;
    }
    let mut out: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    for line in lines {
        let mut cur: Vec<Span<'static>> = Vec::new();
        let mut cur_w: u16 = 0;
        for span in line.spans {
            let style = span.style;
            let mut rest: String = span.content.into_owned();
            while !rest.is_empty() {
                let w = rest.width() as u16;
                if cur_w + w <= width {
                    cur_w += w;
                    cur.push(Span::styled(rest, style));
                    break;
                }
                let (head, tail) = split_at_width_exact(&rest, width - cur_w);
                if head.is_empty() {
                    // A single cell narrower than one wide char: emit it alone.
                    let bi = rest.char_indices().next().map(|(i, _)| i).unwrap_or(0)
                        + rest.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
                    let (ch, tail) = rest.split_at(bi);
                    cur.push(Span::styled(ch.to_string(), style));
                    out.push(Line::from(std::mem::take(&mut cur)));
                    rest = tail.to_string();
                    cur_w = 0;
                    continue;
                }
                cur.push(Span::styled(head, style));
                out.push(Line::from(std::mem::take(&mut cur)));
                rest = tail;
                cur_w = 0;
            }
        }
        out.push(Line::from(cur));
    }
    out
}

fn plain_of(l: &Line<'static>) -> String {
    l.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn common_prefix_chars(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

fn common_suffix_chars(a: &str, b: &str, prefix: usize) -> usize {
    let ac = a.chars().count();
    let bc = b.chars().count();
    let max_s = ac.min(bc).saturating_sub(prefix);
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    let mut s = 0;
    while s < max_s && av[ac - 1 - s] == bv[bc - 1 - s] {
        s += 1;
    }
    s
}

fn detect_shift(old: &[String], new: &[String]) -> i64 {
    let min_len = old.len().min(new.len());
    let score = |d: i64| -> usize {
        (0..new.len())
            .filter(|&i| {
                let j = i as i64 - d;
                j >= 0 && (j as usize) < old.len() && old[j as usize] == new[i]
            })
            .count()
    };
    let mut best_d = 0i64;
    let mut best = score(0);
    let floor = (min_len * 3 / 5).max(2);
    for d in 1..=ANCHOR_SHIFT_MAX {
        for cand in [d, -d] {
            let s = score(cand);
            if s > best && s >= floor {
                best = s;
                best_d = cand;
            }
        }
    }
    best_d
}

fn track_anchor_plain(
    old: &[String],
    new: &[String],
    prev: Option<(usize, u16)>,
) -> Option<(usize, u16)> {
    if old.is_empty() || new.is_empty() {
        return heuristic_anchor(new).or(prev);
    }
    if old == new {
        return prev;
    }
    let d = detect_shift(old, new);
    let prev_shifted = prev.map(|(r, c)| {
        let row = (r as i64 + d).clamp(0, (new.len() - 1) as i64) as usize;
        (row, c)
    });
    let n = old.len().max(new.len());
    let mut diffs: Vec<(usize, usize, usize, usize, String)> = Vec::new();
    for (i, b) in new.iter().enumerate() {
        let j = i as i64 - d;
        let paired = j >= 0 && (j as usize) < old.len();
        if d != 0 && !paired {
            // A confident shift means scroll/insert edges: the unpaired row
            // is scrolled-in content, not an in-place edit at the cursor.
            continue;
        }
        let a = if paired {
            old[j as usize].as_str()
        } else {
            ""
        };
        if a == b.as_str() {
            continue;
        }
        let b = b.as_str();
        let p = common_prefix_chars(a, b);
        let s = common_suffix_chars(a, b, p);
        let alen = a.chars().count();
        let blen = b.chars().count();
        let changed = alen.max(blen).saturating_sub(p + s);
        if changed == 0 {
            continue;
        }
        let limit = ANCHOR_MAX_CHANGE.max(alen.max(blen) / ANCHOR_CHANGE_RATIO);
        if changed > limit {
            continue;
        }
        let grew_rank = if alen != blen { 0 } else { 1 };
        let kept: String = b.chars().take(blen.saturating_sub(s)).collect();
        diffs.push((i, grew_rank, 0usize, changed, kept));
    }
    if n >= 4 && diffs.len() > n / 2 {
        return prev_shifted;
    }
    let mut best: Option<(usize, u16)> = None;
    let mut best_score: (usize, usize, usize) = (usize::MAX, usize::MAX, usize::MAX);
    for (i, grew_rank, _pad, changed, kept) in diffs {
        let col = kept.width() as u16;
        let dist = prev_shifted
            .map(|(r, _)| i.abs_diff(r))
            .unwrap_or(usize::MAX);
        let score = (grew_rank, dist, changed);
        if score < best_score {
            best_score = score;
            best = Some((i, col));
        }
    }
    best.or(prev_shifted)
}

#[cfg(test)]
fn track_anchor(
    old: &[Line<'static>],
    new: &[Line<'static>],
    prev: Option<(usize, u16)>,
) -> Option<(usize, u16)> {
    let old_plain: Vec<String> = old.iter().map(plain_of).collect();
    let new_plain: Vec<String> = new.iter().map(plain_of).collect();
    track_anchor_plain(&old_plain, &new_plain, prev)
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
            anchor: None,
            plain: Vec::new(),
            ver: 1,
            render: RenderCache::default(),
        }];
        terminal
            .draw(|f| draw(f, &mut targets, 3, &None, true))
            .unwrap();
        targets[0].mirror = parse_ansi("host# ");
        targets[0].ver += 1;
        terminal
            .draw(|f| draw(f, &mut targets, 4, &None, true))
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
        let plain: Vec<String> = mirror.iter().map(plain_of).collect();
        let anchor = heuristic_anchor(&plain).unwrap();
        assert_eq!(anchor, (0, width));
        let mirror = with_cursor(&mirror, Some(anchor), true, width);
        let first: String = mirror[0].spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(first.chars().count(), width as usize);
        assert_eq!(mirror[1].spans[0].content, " ");
    }

    #[test]
    fn cursor_visible_in_helix_like_screen() {
        use ratatui::{backend::TestBackend, Terminal as TestTerminal};
        let backend = TestBackend::new(40, 6);
        let mut terminal = TestTerminal::new(backend).unwrap();
        let raw = format!(
            "~\r\n~\r\n~   {}  1 sel  1:1 \r\n{}\r\n",
            "~".repeat(3),
            " ".repeat(40),
        );
        let mut targets = vec![Target {
            pane_id: "t".into(),
            label: "t".into(),
            dead: false,
            mirror: parse_ansi(&raw),
            anchor: None,
            plain: parse_ansi(&raw).iter().map(plain_of).collect(),
            ver: 1,
            render: RenderCache::default(),
        }];
        terminal
            .draw(|f| draw(f, &mut targets, 0, &None, true))
            .unwrap();
        let mut reversed = 0usize;
        for cell in terminal.backend().buffer().content() {
            if cell.modifier.contains(Modifier::REVERSED) {
                reversed += 1;
            }
        }
        assert!(reversed >= 1, "shadow cursor must be visible, got {reversed} reversed cells");
    }

    #[test]
    fn track_anchor_accepts_wide_redraw() {
        let old = vec![
            Line::from(format!("fn main() {{ {} }}", "let x = 1;".repeat(4))),
            Line::from("~"),
        ];
        let mut redrawn = format!("fn main() {{ {} }}", "let x = 2;".repeat(4));
        redrawn.push_str(" /* fmt */");
        let new = vec![Line::from(redrawn), Line::from("~")];
        let anchor = track_anchor(&old, &new, Some((0, 58)));
        assert!(anchor.is_some(), "a proportional redraw within the ratio must still be tracked");
    }

    #[test]
    fn track_anchor_still_rejects_full_line_replacement() {
        let old = vec![
            Line::from("a".repeat(60)),
            Line::from("~"),
        ];
        let new = vec![
            Line::from("b".repeat(60)),
            Line::from("~"),
        ];
        let anchor = track_anchor(&old, &new, Some((1, 1)));
        assert_eq!(anchor, Some((1, 1)), "replacing a whole line must be ignored");
    }

    #[test]
    fn heuristic_anchor_skips_helix_status_line() {
        let plain = vec![
            "fn main() {}".to_string(),
            "~".to_string(),
            "~   xxx  1 sel  1:1 ".to_string(),
            "   ".to_string(),
        ];
        assert_eq!(heuristic_anchor(&plain), Some((1, 1)));
    }

    #[test]
    fn heuristic_anchor_skips_vim_mode_line() {
        let plain = vec!["let x = 1;".to_string(), "-- INSERT --".to_string()];
        assert_eq!(heuristic_anchor(&plain), Some((0, 10)));
    }

    #[test]
    fn heuristic_anchor_keeps_plain_prompt() {
        let plain = vec!["output".to_string(), "host# ".to_string()];
        assert_eq!(heuristic_anchor(&plain), Some((1, 5)));
    }

    #[test]
    fn mirror_wraps_at_source_pane_width() {
        let wrapped = wrap_lines(vec![Line::from("x".repeat(90))], 40);
        assert_eq!(texts(&wrapped), vec!["x".repeat(40), "x".repeat(40), "x".repeat(10)]);
    }

    #[test]
    fn wrap_preserves_span_styles() {
        let line = Line::from(vec![
            Span::styled("aaa", Style::default().bold()),
            Span::styled("bbb", Style::default().italic()),
        ]);
        let wrapped = wrap_lines(vec![line], 5);
        assert_eq!(texts(&wrapped), vec!["aaabb", "b"]);
        assert!(wrapped[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(wrapped[0].spans[1].style.add_modifier.contains(Modifier::ITALIC));
        assert!(wrapped[1].spans[0].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn wrap_never_splits_wide_char() {
        let wrapped = wrap_lines(vec![Line::from("中文中文中文")], 5);
        assert_eq!(texts(&wrapped), vec!["中文", "中文", "中文"]);
    }

    #[test]
    fn cursor_col_maps_to_visual_position() {
        let mirror = wrap_lines(parse_ansi("0123456789X\r\n"), 10);
        assert_eq!(texts(&mirror), vec!["0123456789", "X"]);
        let plain: Vec<String> = mirror.iter().map(plain_of).collect();
        let anchor = heuristic_anchor(&plain).unwrap();
        assert_eq!(anchor, (1, 1));
        let out = with_cursor(&mirror, Some(anchor), true, 10);
        assert!(
            out[1]
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::REVERSED)),
            "the shadow cursor must land on the wrapped continuation row"
        );
    }

    #[test]
    fn track_anchor_follows_typed_char_mid_line() {
        let mut old = vec![
            Line::from("use anyhow::Result;"),
            Line::from("const READ_LINES: usize = 999;"),
            Line::from(" NOR   [scratch] [+]                        1 sel  2:5 "),
        ];
        let new = vec![
            old[0].clone(),
            Line::from("const READ_LINESX: usize = 999;"),
            Line::from(" NOR   [scratch] [+]                        1 sel  2:6 "),
        ];
        let anchor = track_anchor(&old, &new, Some((1, 16)));
        assert_eq!(anchor, Some((1, 17)), "cursor should sit right after the inserted X");
        old = new;
        let _ = old;
    }

    #[test]
    fn track_anchor_prefers_row_near_previous() {
        let old = vec![
            Line::from("let x = 1;"),
            Line::from("~"),
            Line::from(" NOR   file.rs                    1 sel  12:8 "),
        ];
        let new = vec![
            Line::from("let x = 2;"),
            Line::from("~"),
            Line::from(" NOR   file.rs                    1 sel  12:9 "),
        ];
        let anchor = track_anchor(&old, &new, Some((0, 9)));
        assert_eq!(anchor, Some((0, 9)), "text row wins over the far status row");
    }

    #[test]
    fn track_anchor_prefers_inserted_row_over_replaced_status() {
        let old = vec![
            Line::from("    5  use anyhow::Result;"),
            Line::from("    6  use serde::Deserialize;"),
            Line::from(" NOR   src/main.rs                1 sel  5:21 "),
        ];
        let new = vec![
            Line::from("    5  use anyhow::ResultX;"),
            Line::from("    6  use serde::Deserialize;"),
            Line::from(" NOR   src/main.rs                1 sel  5:22 "),
        ];
        let anchor = track_anchor(&old, &new, Some((2, 40))).unwrap();
        assert_eq!(
            anchor.0, 0,
            "text row grew by one char and must win over the status row's same-length counter update"
        );
        assert_eq!(anchor.1, 26, "anchor lands at the width of the kept prefix of the new line");
    }

    #[test]
    fn track_anchor_prefers_shrunk_row_for_backspace() {
        let old = vec![
            Line::from("hello world"),
            Line::from(" NOR   file.rs                1 sel  1:6 "),
        ];
        let new = vec![
            Line::from("hello"),
            Line::from(" NOR   file.rs                1 sel  1:1 "),
        ];
        let anchor = track_anchor(&old, &new, Some((0, 11))).unwrap();
        assert_eq!(anchor, (0, 5), "backspace shrinks text, anchor moves to the kept text width");
    }

    #[test]
    fn track_anchor_ignores_replaced_only_status_when_text_also_changed() {
        let old = vec![
            Line::from("    5  use anyhow::Result;"),
            Line::from(" NOR   src/main.rs                1 sel  5:21 "),
        ];
        let new = vec![
            Line::from("    5  use anyhow::Result;"),
            Line::from(" NOR   src/main.rs                1 sel  5:21 "),
        ];
        let anchor = track_anchor(&old, &new, Some((1, 40)));
        assert_eq!(anchor, Some((1, 40)), "static screen: no diff, keep prev anchor");
    }

    #[test]
    fn track_anchor_follows_full_screen_scroll() {
        let old: Vec<Line<'static>> = (0..20).map(|i| Line::from(format!("line-{i}"))).collect();
        let new: Vec<Line<'static>> = (1..21).map(|i| Line::from(format!("line-{i}"))).collect();
        let anchor = track_anchor(&old, &new, Some((3, 6)));
        assert_eq!(anchor, Some((2, 6)), "a scrolled screen moves the anchor with its content");
    }

    #[test]
    fn track_anchor_follows_scrolled_prompt_up() {
        let old: Vec<Line<'static>> = (1..=5)
            .map(|i| Line::from(format!("out-{i}")))
            .chain([Line::from("host# ")])
            .collect();
        let new: Vec<Line<'static>> = (2..=5)
            .map(|i| Line::from(format!("out-{i}")))
            .chain([Line::from("host# "), Line::from("new output")])
            .collect();
        let anchor = track_anchor(&old, &new, Some((5, 6)));
        assert_eq!(anchor, Some((4, 6)), "the prompt row moved up one row, the anchor must follow");
    }

    #[test]
    fn track_anchor_survives_line_insertion_above() {
        let old = vec![
            Line::from("alpha"),
            Line::from("beta"),
            Line::from("host# typed"),
        ];
        let new = vec![
            Line::from("notice from above"),
            Line::from("alpha"),
            Line::from("beta"),
            Line::from("host# typed"),
        ];
        let anchor = track_anchor(&old, &new, Some((2, 11)));
        assert_eq!(anchor, Some((3, 11)), "insertion above shifts the anchor row, not off it");
    }

    #[test]
    fn track_anchor_survives_line_deletion_above() {
        let old = vec![
            Line::from("notice from above"),
            Line::from("alpha"),
            Line::from("beta"),
            Line::from("host# typed"),
        ];
        let new = vec![
            Line::from("alpha"),
            Line::from("beta"),
            Line::from("host# typed"),
        ];
        let anchor = track_anchor(&old, &new, Some((3, 11)));
        assert_eq!(anchor, Some((2, 11)), "deletion above shifts the anchor row up");
    }

    #[test]
    fn track_anchor_picks_smallest_change_without_prev() {
        let old = vec![
            Line::from("fn main() {"),
            Line::from(" NOR   file.rs                    1 sel  1:1 "),
        ];
        let new = vec![
            Line::from("fn mainX() {"),
            Line::from(" NOR   file.rs                    1 sel  1:2 "),
        ];
        let anchor = track_anchor(&old, &new, None).unwrap();
        assert_eq!(anchor.0, 0, "first-ever diff should land on the text row");
    }

    #[test]
    fn place_block_reverses_char_under_cursor() {
        let mirror = vec![Line::from("abcdef")];
        let out = with_cursor(&mirror, Some((0, 3)), true, 40);
        let spans = &out[0].spans;
        assert_eq!(spans.iter().map(|s| s.content.to_string()).collect::<String>(), "abcdef");
        assert!(spans
            .iter()
            .any(|s| s.content == "d" && s.style.add_modifier.contains(Modifier::REVERSED)));
    }

    #[test]
    fn place_block_appends_space_at_end_of_line() {
        let mirror = vec![Line::from("tmp# ")];
        let out = with_cursor(&mirror, Some((0, 4)), true, 40);
        let flat: String = out[0].spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(flat, "tmp# ");
        assert!(out[0].spans.last().unwrap().style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn place_block_wraps_col_at_cell_width() {
        let mirror = vec![Line::from("aaaa"), Line::from("bbbb")];
        let out = with_cursor(&mirror, Some((0, 4)), true, 4);
        assert!(out[1].spans[0].style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn place_block_handles_double_width_chars() {
        let mirror = vec![Line::from("中文abc")];
        let out = with_cursor(&mirror, Some((0, 2)), true, 40);
        let flat: String = out[0].spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(flat, "中文abc");
        assert!(out[0].spans.iter().any(|s| s.content == "文"));
    }

    #[test]
    fn wheel_maps_scroll_to_arrow_keys_only() {
        use crossterm::event::MouseButton;
        assert_eq!(wheel_key(MouseEventKind::ScrollUp), Some("up"));
        assert_eq!(wheel_key(MouseEventKind::ScrollDown), Some("down"));
        assert_eq!(wheel_key(MouseEventKind::ScrollLeft), Some("left"));
        assert_eq!(wheel_key(MouseEventKind::ScrollRight), Some("right"));
        assert_eq!(wheel_key(MouseEventKind::Down(MouseButton::Left)), None);
        assert_eq!(wheel_key(MouseEventKind::Up(MouseButton::Left)), None);
        assert_eq!(wheel_key(MouseEventKind::Moved), None);
    }

    #[test]
    fn track_anchor_follows_cjk_typing_width() {
        let old = vec![Line::from("你说"), Line::from(" NOR  file.rs   1:2 ")];
        let new = vec![Line::from("你说好"), Line::from(" NOR  file.rs   1:3 ")];
        let anchor = track_anchor(&old, &new, Some((0, 4)));
        assert_eq!(anchor, Some((0, 6)), "one CJK char advances the anchor by 2 columns");
    }
}
