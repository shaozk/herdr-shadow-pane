use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

pub const HERDR_BIN: &str = "herdr";

#[derive(Clone, Debug)]
pub struct Context {
    pub pane_id: String,
    pub tab_id: String,
    pub workspace_id: String,
}

#[derive(Clone, Debug)]
pub struct PaneInfo {
    pub pane_id: String,
    pub agent: Option<String>,
    pub agent_status: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub is_self: bool,
}

#[derive(Deserialize)]
struct PaneCurrent {
    result: PaneCurrentInner,
}

#[derive(Deserialize)]
struct PaneCurrentInner {
    pane: PaneFields,
}

#[derive(Deserialize)]
struct PaneList {
    result: PaneListInner,
}

#[derive(Deserialize)]
struct PaneListInner {
    panes: Vec<PaneFields>,
}

#[derive(Deserialize)]
struct PaneFields {
    pane_id: String,
    tab_id: String,
    workspace_id: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    agent_status: Option<String>,
    #[serde(default)]
    terminal_title_stripped: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

fn run_json(args: &[&str]) -> Result<serde_json::Value> {
    let out = std::process::Command::new(HERDR_BIN)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {HERDR_BIN} {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "{HERDR_BIN} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    serde_json::from_slice(&out.stdout).with_context(|| format!("cannot parse JSON from {HERDR_BIN} {}", args.join(" ")))
}

pub fn resolve_context() -> Result<Context> {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    if let (Some(pane_id), Some(tab_id), Some(workspace_id)) =
        (env("HERDR_PANE_ID"), env("HERDR_TAB_ID"), env("HERDR_WORKSPACE_ID"))
    {
        return Ok(Context { pane_id, tab_id, workspace_id });
    }
    if std::env::var_os("HERDR_ENV").is_none() {
        bail!("not inside a herdr pane: run me inside a herdr-managed pane, or via the plugin action");
    }
    let value = run_json(&["pane", "current", "--current"])?;
    let parsed: PaneCurrent = serde_json::from_value(value).context("unexpected pane current response")?;
    let pane = parsed.result.pane;
    Ok(Context {
        pane_id: pane.pane_id,
        tab_id: pane.tab_id,
        workspace_id: pane.workspace_id,
    })
}

pub fn list_tab_panes(ctx: &Context) -> Result<Vec<PaneInfo>> {
    let value = run_json(&["pane", "list", "--workspace", &ctx.workspace_id])?;
    let parsed: PaneList = serde_json::from_value(value).context("unexpected pane list response")?;
    let mut panes: Vec<PaneInfo> = parsed
        .result
        .panes
        .into_iter()
        .filter(|p| p.tab_id == ctx.tab_id)
        .map(|p| PaneInfo {
            is_self: p.pane_id == ctx.pane_id,
            pane_id: p.pane_id,
            agent: p.agent,
            agent_status: p.agent_status,
            title: p.terminal_title_stripped,
            cwd: p.cwd,
        })
        .collect();
    panes.sort_by_key(|p| p.is_self);
    Ok(panes)
}

fn spawn(args: &[&str]) -> Result<()> {
    let out = std::process::Command::new(HERDR_BIN)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {HERDR_BIN} {}", args.join(" ")))?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

pub fn send_text(pane_id: &str, text: &str) -> Result<()> {
    spawn(&["pane", "send-text", pane_id, text])
}

pub fn send_key(pane_id: &str, key: &str) -> Result<()> {
    spawn(&["pane", "send-keys", pane_id, key])
}

pub fn read_visible(pane_id: &str, lines: usize) -> Result<String> {
    let out = std::process::Command::new(HERDR_BIN)
        .args([
            "pane",
            "read",
            pane_id,
            "--source",
            "visible",
            "--lines",
            &lines.to_string(),
            "--format",
            "text",
        ])
        .output()
        .with_context(|| format!("failed to run {HERDR_BIN} pane read {pane_id}"))?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
