mod broadcast;
mod herdr;
mod herdr_socket;
mod layout;

use anyhow::{bail, Context as _, Result};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

const PLUGIN_ID: &str = "shaozk.herdr-shadow-pane";

fn main() {
    if let Err(err) = run() {
        let _ = restore_terminal();
        eprintln!("herdr-shadow-pane: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("launch") => return launch(),
        Some("--version" | "version") => {
            println!("herdr-shadow-pane {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }
    if let Some(pane) = std::env::var_os("SHADOW_PANE_DEBUG_SOCKET_READ") {
        let mut client = herdr_socket::SocketClient::connect().context("socket connect")?;
        let pane = pane.to_string_lossy().into_owned();
        let text = client.read_visible(&pane, 10)?;
        println!(
            "socket read ok: {} bytes, first line: {:?}",
            text.len(),
            text.lines().next().unwrap_or("")
        );
        return Ok(());
    }
    if std::env::var_os("SHADOW_PANE_DEBUG_LIST").is_some() {
        return debug_list();
    }

    let ctx = herdr::resolve_context()?;
    let panes = herdr::list_tab_panes(&ctx)?;
    let targets: Vec<_> = panes.into_iter().filter(|p| !p.is_self).collect();
    if targets.is_empty() {
        bail!("no other panes in tab {} — nothing to broadcast to", ctx.tab_id);
    }

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend).context("failed to initialize terminal")?;

    enable_raw_mode().context("failed to enable raw mode")?;
    execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture)
        .context("failed to enter alternate screen")?;

    let result = broadcast::run(&mut terminal, targets);

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
