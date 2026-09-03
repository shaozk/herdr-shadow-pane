use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// Persistent client for the herdr socket API (newline-delimited JSON).
///
/// Mirrors the exact request shape the `herdr pane read` CLI sends, so the
/// semantics of `read_visible` match the CLI path byte for byte.
pub struct SocketClient {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl SocketClient {
    pub fn connect() -> Result<Self> {
        let mut last: Option<anyhow::Error> = None;
        for path in candidate_paths() {
            match Self::connect_path(&path) {
                Ok(client) => return Ok(client),
                Err(err) => last = Some(err.context(format!("connect {}", path.display()))),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("no herdr socket path candidate found")))
    }

    pub fn connect_path(path: &std::path::Path) -> Result<Self> {
        let stream = UnixStream::connect(path)?;
        let writer = stream.try_clone().context("clone herdr socket stream")?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            next_id: 0,
        })
    }

    pub fn read_visible(&mut self, pane_id: &str, lines: usize) -> Result<String> {
        let result = self.request(
            "pane.read",
            json!({
                "pane_id": pane_id,
                "source": "visible",
                "lines": lines,
                "format": "ansi",
                "strip_ansi": true,
            }),
        )?;
        let text = result
            .get("read")
            .and_then(|r| r.get("text"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("pane.read response missing read.text"))?;
        Ok(text.to_string())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = format!("shadow-pane:{}", self.next_id);
        let line = serde_json::to_string(&json!({
            "id": id,
            "method": method,
            "params": params,
        }))?;
        writeln!(self.writer, "{line}")?;
        self.writer.flush()?;
        let mut response = String::new();
        let read = self
            .reader
            .read_line(&mut response)
            .context("herdr socket closed")?;
        if read == 0 {
            bail!("herdr socket closed");
        }
        let value: Value = serde_json::from_str(response.trim())?;
        if let Some(error) = value.get("error") {
            let code = error.get("code").and_then(Value::as_str).unwrap_or("error");
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            bail!("{code}: {message}");
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }
}

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|base| base.join("herdr"))
}

/// Discovery mirrors `herdr`'s `active_api_socket_path`: explicit env override
/// first, then the named-session socket, then the default-socket location.
fn candidate_paths() -> Vec<PathBuf> {
    candidate_paths_with(
        std::env::var_os("HERDR_SOCKET_PATH").map(PathBuf::from),
        std::env::var_os("HERDR_SESSION"),
    )
}

fn candidate_paths_with(env_socket: Option<PathBuf>, session: Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(p) = env_socket {
        paths.push(p);
    }
    if let (Some(dir), Some(session)) = (config_dir(), session) {
        paths.push(dir.join("sessions").join(session).join("herdr.sock"));
    }
    if let Some(dir) = config_dir() {
        paths.push(dir.join("herdr.sock"));
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn spawn_responder<F>(f: F) -> (std::path::PathBuf, std::thread::JoinHandle<()>)
    where
        F: Fn(String) -> String + Send + Sync + 'static,
    {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "shadow-pane-socket-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("herdr.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let f = std::sync::Arc::new(f);
        let handle = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let f = f.clone();
                let mut reader = BufReader::new(stream);
                let mut writer = reader.get_ref().try_clone().unwrap();
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    let response = f(line);
                    let _ = writeln!(writer, "{response}");
                    let _ = writer.flush();
                }
            }
        });
        (path, handle)
    }

    #[test]
    fn read_request_parses_pane_read_result() {
        let (path, handle) = spawn_responder(|req| {
            let v: Value = serde_json::from_str(&req).unwrap();
            assert_eq!(v["method"], "pane.read");
            assert_eq!(v["params"]["pane_id"], "w1:p1");
            assert_eq!(v["params"]["format"], "ansi");
            assert_eq!(v["params"]["strip_ansi"], true);
            r#"{"id":"shadow-pane:1","result":{"read":{"text":"host# \u001b[31merr","truncated":false}}}"#.to_string()
        });
        let mut client = SocketClient::connect_path(&path).unwrap();
        let text = client.read_visible("w1:p1", 999).unwrap();
        assert_eq!(text, "host# \u{1b}[31merr");
        handle.join().unwrap();
    }

    #[test]
    fn error_response_surfaces_code() {
        let (path, handle) = spawn_responder(|_| {
            r#"{"id":"x","error":{"code":"pane_not_found","message":"pane not found: w1:p9"}}"#.to_string()
        });
        let mut client = SocketClient::connect_path(&path).unwrap();
        let err = client.read_visible("w1:p9", 999).unwrap_err().to_string();
        assert!(err.contains("pane_not_found"), "got: {err}");
        handle.join().unwrap();
    }

    #[test]
    fn connect_missing_path_falls_through() {
        let err = SocketClient::connect_path(std::path::Path::new("/nonexistent/herdr.sock"));
        assert!(err.is_err());
    }

    #[test]
    fn candidate_paths_honor_env_override() {
        let paths = candidate_paths_with(
            Some(PathBuf::from("/tmp/override.sock")),
            Some(std::ffi::OsString::from("work")),
        );
        assert_eq!(paths[0], PathBuf::from("/tmp/override.sock"));
        let session_sock = paths
            .iter()
            .find(|p| p.ends_with("sessions/work/herdr.sock"));
        assert!(session_sock.is_some(), "session socket must be a candidate");
    }
}
