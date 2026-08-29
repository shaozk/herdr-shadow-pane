mod broadcast;
mod herdr;
mod picker;

use anyhow::{bail, Context as _, Result};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

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
    let status = std::process::Command::new(herdr::HERDR_BIN)
        .args([
            "plugin",
            "pane",
            "open",
            "--plugin",
            PLUGIN_ID,
            "--entrypoint",
            "console",
            "--placement",
            "overlay",
        ])
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
