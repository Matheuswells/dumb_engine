//! Minimal LSP client for rust-analyzer: open/change/save notifications and live diagnostics.

use super::syntax::LiveDiag;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};

/// Is rust-analyzer installed? (The rustup proxy exists even when the component is missing.)
pub fn available() -> bool {
    let mut cmd = Command::new("rust-analyzer");
    cmd.arg("--version").stdout(Stdio::null()).stderr(Stdio::null());
    hide(&mut cmd);
    cmd.status().is_ok_and(|s| s.success())
}

fn hide(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd;
}

/// `rustup component add rust-analyzer` in the background.
pub fn install() -> Receiver<Result<(), String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut cmd = Command::new("rustup");
        cmd.args(["component", "add", "rust-analyzer"]);
        hide(&mut cmd);
        let r = match cmd.output() {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => Err(e.to_string()),
        };
        let _ = tx.send(r);
    });
    rx
}

pub fn path_to_uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_start_matches("//?/");
    let mut out = String::from("file:///");
    for c in s.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '#' => out.push_str("%23"),
            '%' => out.push_str("%25"),
            c => out.push(c),
        }
    }
    out
}

pub fn uri_to_path(uri: &str) -> PathBuf {
    let s = uri.trim_start_matches("file://").trim_start_matches('/');
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v as char);
                i += 3;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    PathBuf::from(out.replace('/', "\\"))
}

/// UTF-16 column (LSP) to char column on a line.
fn char_col(line: &str, utf16: usize) -> usize {
    let mut n = 0;
    for (ci, ch) in line.chars().enumerate() {
        if n >= utf16 {
            return ci;
        }
        n += ch.len_utf16();
    }
    line.chars().count()
}

pub struct Lsp {
    child: Child,
    to_server: Sender<String>,
    from_server: Receiver<Value>,
    next_id: i64,
    versions: HashMap<PathBuf, i64>,
    pub ready: bool,
    /// Progress text (indexing, building proc macros...).
    pub status: String,
    pub diagnostics: HashMap<PathBuf, Vec<(usize, usize, usize, usize, bool, String)>>,
    pub changed: bool,
}

impl Lsp {
    pub fn start(root: &Path) -> Result<Self, String> {
        let mut cmd = Command::new("rust-analyzer");
        cmd.current_dir(root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        hide(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("rust-analyzer: {e}"))?;
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (to_server, rx_out) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for body in rx_out {
                if write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).is_err() || stdin.flush().is_err() {
                    break;
                }
            }
        });
        let (tx_in, from_server) = std::sync::mpsc::channel::<Value>();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 {
                        return;
                    }
                    let h = h.trim();
                    if h.is_empty() {
                        break;
                    }
                    if let Some(v) = h.strip_prefix("Content-Length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut buf = vec![0u8; len];
                if r.read_exact(&mut buf).is_err() {
                    return;
                }
                if let Ok(v) = serde_json::from_slice::<Value>(&buf) {
                    if tx_in.send(v).is_err() {
                        return;
                    }
                }
            }
        });
        let mut lsp = Lsp {
            child,
            to_server,
            from_server,
            next_id: 1,
            versions: HashMap::new(),
            ready: false,
            status: "starting".into(),
            diagnostics: HashMap::new(),
            changed: false,
        };
        let root_uri = path_to_uri(root);
        lsp.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": { "publishDiagnostics": { "relatedInformation": false } },
                    "window": { "workDoneProgress": true }
                },
                "initializationOptions": { "checkOnSave": true }
            }),
        );
        Ok(lsp)
    }

    fn send(&self, v: Value) {
        let _ = self.to_server.send(v.to_string());
    }

    fn request(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        id
    }

    fn notify(&self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    pub fn did_open_or_change(&mut self, path: &Path, text: &str) {
        let uri = path_to_uri(path);
        match self.versions.get_mut(path) {
            None => {
                self.versions.insert(path.to_path_buf(), 1);
                self.notify("textDocument/didOpen", json!({ "textDocument": { "uri": uri, "languageId": "rust", "version": 1, "text": text } }));
            }
            Some(v) => {
                *v += 1;
                let v = *v;
                self.notify("textDocument/didChange", json!({ "textDocument": { "uri": uri, "version": v }, "contentChanges": [{ "text": text }] }));
            }
        }
    }

    pub fn did_save(&self, path: &Path) {
        self.notify("textDocument/didSave", json!({ "textDocument": { "uri": path_to_uri(path) } }));
    }

    /// Handle everything the server sent. `text_of` gives buffer text for column conversion.
    pub fn poll(&mut self, text_of: &dyn Fn(&Path) -> Option<String>) {
        while let Ok(m) = self.from_server.try_recv() {
            let method = m.get("method").and_then(Value::as_str);
            match method {
                Some("textDocument/publishDiagnostics") => {
                    let p = &m["params"];
                    let path = uri_to_path(p["uri"].as_str().unwrap_or(""));
                    let text = text_of(&path).unwrap_or_default();
                    let lines: Vec<&str> = text.lines().collect();
                    let mut out = Vec::new();
                    for d in p["diagnostics"].as_array().cloned().unwrap_or_default() {
                        let r = &d["range"];
                        let pos = |k: &str| (r[k]["line"].as_u64().unwrap_or(0) as usize, r[k]["character"].as_u64().unwrap_or(0) as usize);
                        let ((l0, c0), (l1, c1)) = (pos("start"), pos("end"));
                        let cc = |l: usize, c: usize| lines.get(l).map_or(c, |s| char_col(s, c));
                        let sev = d["severity"].as_u64().unwrap_or(1);
                        if sev > 2 {
                            continue; // info / hints
                        }
                        let msg = d["message"].as_str().unwrap_or("").to_string();
                        out.push((l0, cc(l0, c0), l1, cc(l1, c1), sev == 1, msg));
                    }
                    self.diagnostics.insert(path, out);
                    self.changed = true;
                }
                Some("$/progress") => {
                    let v = &m["params"]["value"];
                    match v["kind"].as_str() {
                        Some("end") => self.status = "ready".into(),
                        _ => {
                            let t = v["title"].as_str().or_else(|| v["message"].as_str()).unwrap_or("");
                            let pct = v["percentage"].as_u64().map(|p| format!(" {p}%")).unwrap_or_default();
                            if !t.is_empty() {
                                self.status = format!("{t}{pct}");
                            }
                        }
                    }
                }
                Some(_) if m.get("id").is_some() => {
                    // Server requests (workDoneProgress/create, configuration...): reply null.
                    let reply = if method == Some("workspace/configuration") {
                        let n = m["params"]["items"].as_array().map_or(1, Vec::len);
                        json!({ "jsonrpc": "2.0", "id": m["id"], "result": vec![Value::Null; n] })
                    } else {
                        json!({ "jsonrpc": "2.0", "id": m["id"], "result": null })
                    };
                    self.send(reply);
                }
                None if m.get("id").and_then(Value::as_i64) == Some(1) => {
                    self.ready = true;
                    self.status = "ready".into();
                    self.notify("initialized", json!({}));
                }
                _ => {}
            }
        }
    }

    /// Diagnostics for a file as `LiveDiag`s.
    pub fn diags_for(&self, path: &Path) -> Vec<LiveDiag> {
        let n = |p: &Path| p.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
        let key = n(path);
        self.diagnostics
            .iter()
            .filter(|(p, _)| n(p) == key)
            .flat_map(|(_, v)| v.iter())
            .map(|(l0, c0, l1, c1, err, msg)| LiveDiag { line: *l0, col: *c0, end_line: *l1, end_col: *c1, error: *err, text: msg.clone(), source: "rust-analyzer" })
            .collect()
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        let _ = self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        let _ = self.child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip() {
        let p = Path::new(r"C:\Users\me\My Game\Scripts\src\move.rs");
        let u = path_to_uri(p);
        assert_eq!(u, "file:///C:/Users/me/My%20Game/Scripts/src/move.rs");
        assert_eq!(uri_to_path(&u), p);
        assert_eq!(uri_to_path("file:///c%3A/x/y.rs"), Path::new(r"c:\x\y.rs"));
        assert_eq!(char_col("aé😀b", 4), 3);
    }
}
