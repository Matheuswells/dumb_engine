//! MCP over Streamable HTTP on localhost.
//!
//! Requests are handled on worker threads. `initialize`, `ping` and `tools/list` are answered
//! there; every `tools/call` becomes a [`Call`] that the editor's main loop picks up with
//! [`Server::poll`], runs between frames and answers through [`Call::reply`].

use super::{tools, Content, ToolResult};
use serde_json::{json, Map, Value as Json};
use std::io::Read;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// Longest a tool call may take (a script build or an export with `wait`).
const CALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_BODY: u64 = 32 << 20;

/// One `tools/call` waiting for the editor.
pub struct Call {
    pub tool: String,
    pub args: Map<String, Json>,
    reply: Sender<ToolResult>,
}

impl Call {
    pub fn new(tool: &str, args: Map<String, Json>) -> (Call, Receiver<ToolResult>) {
        let (reply, rx) = channel();
        (Call { tool: tool.into(), args, reply }, rx)
    }

    pub fn reply(self, r: ToolResult) {
        let _ = self.reply.send(r);
    }
}

pub struct Server {
    pub port: u16,
    http: Arc<tiny_http::Server>,
    rx: Receiver<Call>,
}

impl Server {
    pub fn start(port: u16) -> Result<Server, String> {
        let http = Arc::new(tiny_http::Server::http(("127.0.0.1", port)).map_err(|e| format!("could not listen on 127.0.0.1:{port}: {e}"))?);
        let port = http.server_addr().to_ip().map_or(port, |a| a.port());
        let (tx, rx) = channel();
        let h = http.clone();
        std::thread::Builder::new()
            .name("mcp-accept".into())
            .spawn(move || {
                for req in h.incoming_requests() {
                    let tx = tx.clone();
                    let _ = std::thread::Builder::new().name("mcp-request".into()).spawn(move || handle_http(req, &tx));
                }
            })
            .map_err(|e| e.to_string())?;
        log::info!("MCP server listening on http://127.0.0.1:{port}/mcp");
        Ok(Server { port, http, rx })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }

    /// Tool calls received since the last poll.
    pub fn poll(&self) -> Vec<Call> {
        self.rx.try_iter().collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.http.unblock();
    }
}

/// Browsers send `Origin`; only local pages may talk to the editor (DNS-rebinding guard).
fn origin_allowed(origin: Option<&str>) -> bool {
    let Some(o) = origin else { return true };
    if o == "null" {
        return false;
    }
    let host = o.split("://").nth(1).unwrap_or(o);
    let host = host.split('/').next().unwrap_or(host);
    let host = if host.starts_with('[') { host.split(']').next().map(|h| &h[1..]).unwrap_or(host) } else { host.split(':').next().unwrap_or(host) };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn header<'a>(req: &'a tiny_http::Request, name: &str) -> Option<&'a str> {
    req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str())
}

fn handle_http(mut req: tiny_http::Request, tx: &Sender<Call>) {
    use tiny_http::{Header, Method, Response};
    let json_header = Header::from_bytes("Content-Type", "application/json").unwrap();
    let path = req.url().split('?').next().unwrap_or("").to_string();
    if path != "/mcp" && path != "/" {
        let _ = req.respond(Response::from_string("not found; the MCP endpoint is /mcp").with_status_code(404));
        return;
    }
    if !origin_allowed(header(&req, "Origin")) {
        let _ = req.respond(Response::from_string("forbidden origin").with_status_code(403));
        return;
    }
    match req.method() {
        Method::Post => {}
        Method::Get | Method::Delete => {
            // No server-initiated stream and no sessions to end.
            let allow = Header::from_bytes("Allow", "POST").unwrap();
            let _ = req.respond(Response::empty(405).with_header(allow));
            return;
        }
        _ => {
            let _ = req.respond(Response::empty(405));
            return;
        }
    }
    let mut body = String::new();
    if let Err(e) = req.as_reader().take(MAX_BODY).read_to_string(&mut body) {
        let _ = req.respond(Response::from_string(format!("bad body: {e}")).with_status_code(400));
        return;
    }
    let dispatch = |tool: &str, args: Map<String, Json>| -> ToolResult {
        let (call, rx) = Call::new(tool, args);
        tx.send(call).map_err(|_| "the editor is shutting down".to_string())?;
        rx.recv_timeout(CALL_TIMEOUT).unwrap_or_else(|_| Err("the editor did not answer (it may have closed the project)".into()))
    };
    let out = match serde_json::from_str::<Json>(&body) {
        Ok(Json::Array(batch)) => {
            let replies: Vec<Json> = batch.iter().filter_map(|m| handle_message(m, &dispatch)).collect();
            (!replies.is_empty()).then_some(Json::Array(replies))
        }
        Ok(msg) => handle_message(&msg, &dispatch),
        Err(e) => Some(error(Json::Null, -32700, &format!("parse error: {e}"))),
    };
    let _ = match out {
        Some(reply) => req.respond(Response::from_string(reply.to_string()).with_header(json_header)),
        // Only notifications/responses: accepted, nothing to say.
        None => req.respond(Response::empty(202)),
    };
}

fn error(id: Json, code: i64, message: &str) -> Json {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

const INSTRUCTIONS: &str = "Controls the running Dumb Engine editor. Start with get_editor_state. \
Entities are referred to by numeric id (from list_entities) or by unique name. Assets are referred to \
by path under Assets/ (e.g. \"Materials/Gold.mat\"), UUID, or built-in name (Cube, Sphere, Plane, Cylinder). \
Component values use plain JSON: vectors/quaternions/colors are arrays, enums are variant names. \
Edits go through the editor's undo history and mark scenes dirty; call save_scene to write them. \
Use capture_view to see the scene or game view.";

/// Handle one JSON-RPC message. `None` for notifications and responses.
pub fn handle_message(msg: &Json, dispatch: &dyn Fn(&str, Map<String, Json>) -> ToolResult) -> Option<Json> {
    let method = msg.get("method")?.as_str()?;
    let id = msg.get("id").cloned()?;
    let params = msg.get("params").cloned().unwrap_or(Json::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Json::as_str).unwrap_or("");
            let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&PROTOCOL_VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "dumb-engine", "title": "Dumb Engine Editor", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools::definitions() }),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Json::as_str) else {
                return Some(error(id, -32602, "tools/call needs a `name`"));
            };
            if !tools::exists(name) {
                return Some(error(id, -32602, &format!("unknown tool `{name}`")));
            }
            let args = match params.get("arguments") {
                None | Some(Json::Null) => Map::new(),
                Some(Json::Object(m)) => m.clone(),
                Some(_) => return Some(error(id, -32602, "`arguments` must be an object")),
            };
            match dispatch(name, args) {
                Ok(content) => json!({ "content": content.iter().map(Content::to_json).collect::<Vec<_>>(), "isError": false }),
                Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
            }
        }
        _ => return Some(error(id, -32601, &format!("method `{method}` not found"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo(tool: &str, args: Map<String, Json>) -> ToolResult {
        if tool == "undo" {
            return Err("nothing to undo".into());
        }
        Ok(vec![Content::Text(format!("{tool} {}", Json::Object(args)))])
    }

    #[test]
    fn initialize_negotiates_version() {
        let r = handle_message(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}), &echo).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        let r = handle_message(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}), &echo).unwrap();
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[test]
    fn notifications_get_no_reply() {
        assert!(handle_message(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}), &echo).is_none());
    }

    #[test]
    fn tool_calls_dispatch_and_report_errors() {
        let r = handle_message(&json!({"jsonrpc":"2.0","id":"a","method":"tools/call","params":{"name":"play","arguments":{"x":1}}}), &echo).unwrap();
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["content"][0]["text"], "play {\"x\":1}");
        let r = handle_message(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"undo"}}), &echo).unwrap();
        assert_eq!(r["result"]["isError"], true);
        let r = handle_message(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nope"}}), &echo).unwrap();
        assert_eq!(r["error"]["code"], -32602);
        let r = handle_message(&json!({"jsonrpc":"2.0","id":4,"method":"bogus"}), &echo).unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }

    #[test]
    fn origins() {
        assert!(origin_allowed(None));
        assert!(origin_allowed(Some("http://localhost:3000")));
        assert!(origin_allowed(Some("http://127.0.0.1")));
        assert!(origin_allowed(Some("http://[::1]:8080")));
        assert!(!origin_allowed(Some("https://evil.example")));
        assert!(!origin_allowed(Some("http://localhost.evil.example")));
        assert!(!origin_allowed(Some("null")));
    }

    #[test]
    fn http_round_trip() {
        let server = Server::start(0).unwrap();
        let url = server.url();
        let client = std::thread::spawn(move || {
            let post = |body: &str| -> (u16, String) {
                let addr = url.trim_start_matches("http://").trim_end_matches("/mcp").to_string();
                let mut s = std::net::TcpStream::connect(&addr).unwrap();
                use std::io::Write;
                write!(s, "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                let mut out = String::new();
                s.read_to_string(&mut out).unwrap();
                let code = out[9..12].parse().unwrap();
                (code, out.split("\r\n\r\n").nth(1).unwrap_or("").to_string())
            };
            let (code, _) = post(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
            assert_eq!(code, 202);
            let (code, body) = post(r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"get_editor_state"}}"#);
            assert_eq!(code, 200);
            body
        });
        // Play the editor's side: answer the call from the "main loop".
        let call = loop {
            if let Some(c) = server.poll().pop() {
                break c;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(call.tool, "get_editor_state");
        call.reply(Ok(vec![Content::Text("hi".into())]));
        let body: Json = serde_json::from_str(&client.join().unwrap()).unwrap();
        assert_eq!(body["id"], 7);
        assert_eq!(body["result"]["content"][0]["text"], "hi");
    }
}
