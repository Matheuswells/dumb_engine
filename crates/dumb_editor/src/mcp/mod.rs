//! Model Context Protocol server: lets AI agents (Claude Code, Claude Desktop, any MCP
//! client) drive the running editor. See `docs/MCP.md`.

pub mod convert;
pub mod server;
pub mod tools;

use serde_json::{json, Map, Value as Json};

pub use server::{Call, Server};

use crate::prefs::EditorSettings;
use std::path::PathBuf;
use std::sync::Mutex;

pub const DEFAULT_PORT: u16 = 47100;

static STATUS: Mutex<String> = Mutex::new(String::new());

/// One-line server state for the Preferences window.
pub fn status() -> String {
    STATUS.lock().map(|s| s.clone()).unwrap_or_default()
}

fn set_status(s: String) {
    if let Ok(mut g) = STATUS.lock() {
        *g = s;
    }
}

/// Owns the server and keeps it in line with the MCP preferences.
#[derive(Default)]
pub struct Host {
    server: Option<Server>,
    /// (enabled, port) last applied, so a failed start is not retried every frame.
    applied: Option<(bool, u16)>,
}

impl Host {
    /// Start, stop or move the server when the preferences change.
    pub fn sync(&mut self, settings: &EditorSettings) {
        let want = (settings.mcp_enabled, settings.mcp_port);
        if self.applied == Some(want) {
            return;
        }
        self.applied = Some(want);
        self.server = None;
        if !want.0 {
            set_status("off".into());
            return;
        }
        match Server::start(want.1) {
            Ok(s) => {
                set_status(format!("listening on {}", s.url()));
                self.server = Some(s);
            }
            Err(e) => {
                log::error!("MCP server: {e}");
                set_status(format!("not running: {e}"));
            }
        }
    }

    pub fn poll(&self) -> Vec<Call> {
        self.server.as_ref().map(Server::poll).unwrap_or_default()
    }
}

/// Answer a call while no project is open (start screen). Returns a project to open.
pub fn handle_without_project(call: Call, settings: &mut EditorSettings) -> Option<PathBuf> {
    let mut open = None;
    let r = (|| {
        let a = Args(&call.args);
        let recent = || crate::project_manager::recent_projects().iter().map(|p| p.display().to_string()).collect::<Vec<_>>();
        match call.tool.as_str() {
            "get_editor_state" => ok(json!({ "project": null, "recent_projects": recent(), "hint": "No project is open. Use open_project or create_project." })),
            "list_recent_projects" => ok(json!(recent())),
            "open_project" | "create_project" => {
                let dir = PathBuf::from(a.req_str("path")?);
                if call.tool == "create_project" {
                    if crate::project_manager::is_project(&dir) {
                        return Err(format!("{} already contains a project; use open_project", dir.display()));
                    }
                    let name = a.str("name")?.map(str::to_string).unwrap_or_else(|| dir.file_name().map_or("Project".into(), |n| n.to_string_lossy().into_owned()));
                    crate::project_manager::create_project(&dir, &name).map_err(|e| e.to_string())?;
                } else if !crate::project_manager::is_project(&dir) {
                    return Err(format!("{} is not a Dumb Engine project (no project.ron)", dir.display()));
                }
                open = Some(dir.clone());
                done(format!("Opening {}", dir.display()))
            }
            "get_editor_settings" => serde_json::to_value(&*settings).map_err(|e| e.to_string()).and_then(ok),
            "set_editor_settings" => {
                let patch = Json::Object(a.object("settings")?.ok_or("missing argument `settings`")?.clone());
                *settings = convert::patch_serde(settings, &patch)?;
                settings.save();
                serde_json::to_value(&*settings).map_err(|e| e.to_string()).and_then(ok)
            }
            t => Err(format!("`{t}` needs an open project. Use open_project or create_project first.")),
        }
    })();
    call.reply(r);
    open
}

/// One item of a tool result.
pub enum Content {
    Text(String),
    /// PNG bytes.
    Image(Vec<u8>),
}

impl Content {
    pub fn json(v: &Json) -> Content {
        Content::Text(serde_json::to_string_pretty(v).unwrap_or_default())
    }

    fn to_json(&self) -> Json {
        match self {
            Content::Text(t) => json!({ "type": "text", "text": t }),
            Content::Image(png) => {
                use base64::Engine;
                json!({ "type": "image", "mimeType": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(png) })
            }
        }
    }
}

pub type ToolResult = Result<Vec<Content>, String>;

/// A JSON tool result.
pub fn ok(v: Json) -> ToolResult {
    Ok(vec![Content::json(&v)])
}

/// A one-line text result.
pub fn done(msg: impl Into<String>) -> ToolResult {
    Ok(vec![Content::Text(msg.into())])
}

/// Typed access to tool arguments with readable errors.
pub struct Args<'a>(pub &'a Map<String, Json>);

impl<'a> Args<'a> {
    pub fn get(&self, k: &str) -> Option<&'a Json> {
        self.0.get(k).filter(|v| !v.is_null())
    }

    pub fn req(&self, k: &str) -> Result<&'a Json, String> {
        self.get(k).ok_or_else(|| format!("missing argument `{k}`"))
    }

    pub fn str(&self, k: &str) -> Result<Option<&'a str>, String> {
        match self.get(k) {
            None => Ok(None),
            Some(Json::String(s)) => Ok(Some(s)),
            Some(v) => Err(format!("`{k}` must be a string, got {v}")),
        }
    }

    pub fn req_str(&self, k: &str) -> Result<&'a str, String> {
        self.str(k)?.ok_or_else(|| format!("missing argument `{k}`"))
    }

    pub fn bool(&self, k: &str) -> Result<Option<bool>, String> {
        match self.get(k) {
            None => Ok(None),
            Some(Json::Bool(b)) => Ok(Some(*b)),
            Some(v) => Err(format!("`{k}` must be true or false, got {v}")),
        }
    }

    pub fn f64(&self, k: &str) -> Result<Option<f64>, String> {
        match self.get(k) {
            None => Ok(None),
            Some(v) => v.as_f64().map(Some).ok_or_else(|| format!("`{k}` must be a number, got {v}")),
        }
    }

    pub fn usize(&self, k: &str) -> Result<Option<usize>, String> {
        match self.get(k) {
            None => Ok(None),
            Some(v) => v.as_u64().map(|n| Some(n as usize)).ok_or_else(|| format!("`{k}` must be a non-negative integer, got {v}")),
        }
    }

    pub fn floats<const N: usize>(&self, k: &str) -> Result<Option<[f32; N]>, String> {
        let Some(v) = self.get(k) else { return Ok(None) };
        let a = v.as_array().filter(|a| a.len() == N).ok_or_else(|| format!("`{k}` must be an array of {N} numbers"))?;
        let mut out = [0.0; N];
        for (o, x) in out.iter_mut().zip(a) {
            *o = x.as_f64().ok_or_else(|| format!("`{k}` must contain numbers"))? as f32;
        }
        Ok(Some(out))
    }

    pub fn vec3(&self, k: &str) -> Result<Option<dumb_core::Vec3>, String> {
        Ok(self.floats::<3>(k)?.map(dumb_core::Vec3::from))
    }

    pub fn strings(&self, k: &str) -> Result<Vec<String>, String> {
        match self.get(k) {
            None => Ok(Vec::new()),
            Some(Json::Array(a)) => a.iter().map(|v| v.as_str().map(str::to_string).ok_or_else(|| format!("`{k}` must contain strings"))).collect(),
            Some(v) => Err(format!("`{k}` must be an array of strings, got {v}")),
        }
    }

    pub fn object(&self, k: &str) -> Result<Option<&'a Map<String, Json>>, String> {
        match self.get(k) {
            None => Ok(None),
            Some(Json::Object(o)) => Ok(Some(o)),
            Some(v) => Err(format!("`{k}` must be an object, got {v}")),
        }
    }
}

/// Encode RGBA8 pixels as PNG, downscaling to `max_width`.
pub fn encode_png(width: u32, height: u32, rgba: Vec<u8>, max_width: u32) -> Result<Vec<u8>, String> {
    let mut img = image::RgbaImage::from_raw(width, height, rgba).ok_or("bad image size")?;
    // The view is opaque; drop alpha noise so viewers don't show a checkerboard.
    for p in img.pixels_mut() {
        p.0[3] = 255;
    }
    let img = if max_width > 0 && width > max_width {
        let h = (height as u64 * max_width as u64 / width as u64).max(1) as u32;
        image::imageops::resize(&img, max_width, h, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    #[test]
    fn png_downscales() {
        let png = super::encode_png(4, 2, vec![200; 32], 2).unwrap();
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!((img.width(), img.height()), (2, 1));
    }
}
