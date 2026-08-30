mod broadcast;
mod herdr;
mod layout;
mod picker;

use anyhow::{bail, Context as _, Result};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use crate::herdr::Rect;

const PLUGIN_ID: &str = "shaozk.sync-panes";

fn main() {
    if let Err(err) = run() {
        let _ = restore_terminal();
        eprintln!("sync-panes: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("launch") => return launch(),
        Some("--version" | "version") => {
            println!("sync-panes {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }
    if std::env::var_os("SYNC_PANES_DEBUG_LIST").is_some() {
        return debug_list();
    }
    if let Ok(master) = std::env::var("SYNC_PANES_DEBUG_ALIGN") {
        return debug_align(&master);
    }

    let ctx = herdr::resolve_context()?;
    let panes = herdr::list_tab_panes(&ctx)?;

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend).context("failed to initialize terminal")?;

    enable_raw_mode().context("failed to enable raw mode")?;
    execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture)
        .context("failed to enter alternate screen")?;

    let result = (|| {
        let selected = picker::run(&mut terminal, &ctx, panes)?;
        match selected {
            Some(targets) if !targets.is_empty() => {
                broadcast::run(&mut terminal, targets)?;
                Ok(())
            }
            _ => Ok(()),
        }
    })();

    restore_terminal()?;
    result
}

fn restore_terminal() -> Result<()> {
    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

fn launch() -> Result<()> {
    let mut cmd = std::process::Command::new(herdr::HERDR_BIN);
    cmd.args([
        "plugin",
        "pane",
        "open",
        "--plugin",
        PLUGIN_ID,
        "--entrypoint",
        "console",
        "--placement",
        "overlay",
    ]);
    if let Ok(master) = herdr::focused_pane_id() {
        cmd.arg("--env").arg(format!("SYNC_PANES_MASTER={master}"));
    }
    let status = cmd
        .status()
        .context("failed to run herdr (is it on PATH?)")?;
    if !status.success() {
        bail!("herdr plugin pane open failed with {status}");
    }
    Ok(())
}

fn debug_list() -> Result<()> {
    let ctx = herdr::resolve_context()?;
    let panes = herdr::list_tab_panes(&ctx)?;
    println!("context: tab={} workspace={} self={}", ctx.tab_id, ctx.workspace_id, ctx.pane_id);
    for p in panes {
        println!(
            "  {}{} agent={:?} status={:?} title={:?}",
            p.pane_id,
            if p.is_self { " (self)" } else { "" },
            p.agent,
            p.agent_status,
            p.title
        );
    }
    Ok(())
}

fn debug_align(master: &str) -> Result<()> {
    let layout0 = herdr::tab_layout(master)?;
    let goal = layout::rect_of(&layout0, master).context("master pane not found in layout")?;
    let self_id = std::env::var("HERDR_PANE_ID").ok();
    let goals: Vec<(String, Rect)> = layout0
        .panes
        .iter()
        .map(|(id, _)| id.clone())
        .filter(|id| id != master && Some(id) != self_id.as_ref())
        .map(|id| (id, goal))
        .collect();
    println!("master goal: {}x{}", goal.width, goal.height);
    for (id, r) in &goals {
        println!("  before {id}: {}x{}", r.width, r.height);
    }
    layout::drive_all(&goals)?;
    let after = herdr::tab_layout(master)?;
    for (id, g) in &goals {
        let r = layout::rect_of(&after, id).unwrap_or_default();
        println!(
            "  after  {id}: {}x{} (goal {}x{}, aligned={})",
            r.width,
            r.height,
            g.width,
            g.height,
            layout::aligned(&after, id, *g)
        );
    }
    Ok(())
}
