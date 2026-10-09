//! Web pages inside the engine window: HTML/CSS/JS, https, video, GIF/PNG, through the
//! system web view (WebView2 on Windows) as child windows placed over the game or editor.
//!
//! The host collects `WebRequest`s each frame (HUD web panels, the editor's browser window)
//! and calls [`WebLayer::sync`]: panels are created, moved or loaded as needed and destroyed
//! when nobody asks for them any more. Pages talk to the game with
//! `window.ipc.postMessage("text")`; the game runs JavaScript in them with [`WebLayer::eval`].
//!
//! Native web views are drawn on top of everything in their window area (egui popups can't
//! cover them).

use crate::hud_view::WebRequest;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct Entry {
    view: wry::WebView,
    url: String,
    bounds: [i32; 4],
    seen: bool,
}

pub struct WebLayer {
    views: HashMap<String, Entry>,
    project_root: PathBuf,
    messages: Arc<Mutex<Vec<(String, String)>>>,
    /// Last error (shown by the host), e.g. WebView2 missing.
    pub error: Option<String>,
}

/// What to load: a URL, or inline HTML.
#[derive(Debug, PartialEq)]
pub enum Source {
    Url(String),
    Html(String),
}

/// Resolve a page reference: `https://...`, `file:///...`, inline `<html>...`, or a path in the
/// project (`Assets/UI/menu.html` or `UI/menu.html`).
pub fn resolve(url: &str, project_root: &Path) -> Source {
    let t = url.trim();
    if t.starts_with('<') {
        return Source::Html(t.to_string());
    }
    let lower = t.to_ascii_lowercase();
    if ["http://", "https://", "file:", "data:", "about:"].iter().any(|p| lower.starts_with(p)) {
        return Source::Url(t.to_string());
    }
    if lower.starts_with("www.") {
        return Source::Url(format!("https://{t}"));
    }
    let p = Path::new(t);
    let candidates = [project_root.join(p), project_root.join("Assets").join(p), p.to_path_buf()];
    let file = candidates.iter().find(|c| c.exists()).cloned().unwrap_or_else(|| project_root.join("Assets").join(p));
    let abs = file.canonicalize().unwrap_or(file);
    let s = crate::strip_unc(&abs).to_string_lossy().replace('\\', "/").replace(' ', "%20");
    Source::Url(format!("file:///{}", s.trim_start_matches('/')))
}

impl WebLayer {
    pub fn new(project_root: &Path) -> Self {
        WebLayer { views: HashMap::new(), project_root: project_root.to_path_buf(), messages: Default::default(), error: None }
    }

    /// Create/update/remove web views to match `requests` (rects in egui points).
    pub fn sync<W: wry::raw_window_handle::HasWindowHandle>(&mut self, window: &W, requests: &[WebRequest], pixels_per_point: f32) {
        for e in self.views.values_mut() {
            e.seen = false;
        }
        for r in requests {
            let b = [
                (r.rect.left() * pixels_per_point).round() as i32,
                (r.rect.top() * pixels_per_point).round() as i32,
                (r.rect.width() * pixels_per_point).round().max(1.0) as i32,
                (r.rect.height() * pixels_per_point).round().max(1.0) as i32,
            ];
            let bounds = wry::Rect {
                position: wry::dpi::PhysicalPosition::new(b[0], b[1]).into(),
                size: wry::dpi::PhysicalSize::new(b[2] as u32, b[3] as u32).into(),
            };
            match self.views.get_mut(&r.id) {
                Some(e) => {
                    e.seen = true;
                    if e.bounds != b {
                        let _ = e.view.set_bounds(bounds);
                        e.bounds = b;
                    }
                    if e.url != r.url {
                        match resolve(&r.url, &self.project_root) {
                            Source::Url(u) => {
                                let _ = e.view.load_url(&u);
                            }
                            Source::Html(h) => {
                                let js = format!("document.open();document.write({});document.close();", serde_json_string(&h));
                                let _ = e.view.evaluate_script(&js);
                            }
                        }
                        e.url = r.url.clone();
                    }
                }
                None => {
                    let id = r.id.clone();
                    let msgs = self.messages.clone();
                    let mut builder = wry::WebViewBuilder::new()
                        .with_bounds(bounds)
                        .with_autoplay(true)
                        // Native child windows can't blend over the Vulkan swapchain: panels are opaque.
                        .with_background_color((18, 20, 26, 255))
                        .with_devtools(true)
                        .with_ipc_handler(move |req| {
                            if let Ok(mut m) = msgs.lock() {
                                m.push((id.clone(), req.body().clone()));
                            }
                        });
                    builder = match resolve(&r.url, &self.project_root) {
                        Source::Url(u) => builder.with_url(u),
                        Source::Html(h) => builder.with_html(h),
                    };
                    match builder.build_as_child(window) {
                        Ok(view) => {
                            self.views.insert(r.id.clone(), Entry { view, url: r.url.clone(), bounds: b, seen: true });
                            self.error = None;
                        }
                        Err(e) => {
                            if self.error.is_none() {
                                log::error!("web view: {e}");
                            }
                            self.error = Some(format!("web view unavailable: {e}"));
                        }
                    }
                }
            }
        }
        self.views.retain(|_, e| e.seen);
    }

    /// Run JavaScript in a web panel.
    pub fn eval(&self, id: &str, js: &str) {
        if let Some(e) = self.views.get(id) {
            let _ = e.view.evaluate_script(js);
        }
    }

    pub fn back(&self, id: &str) {
        self.eval(id, "history.back()");
    }

    pub fn forward(&self, id: &str) {
        self.eval(id, "history.forward()");
    }

    pub fn reload(&self, id: &str) {
        self.eval(id, "location.reload()");
    }

    pub fn open_devtools(&self, id: &str) {
        if let Some(e) = self.views.get(id) {
            e.view.open_devtools();
        }
    }

    /// Messages posted by pages since the last call: (web id, text).
    pub fn take_messages(&self) -> Vec<(String, String)> {
        self.messages.lock().map(|mut m| std::mem::take(&mut *m)).unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.views.len()
    }

    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }

    /// Remove every web view (closing a project, stopping play).
    pub fn clear(&mut self) {
        self.views.clear();
    }
}

fn serde_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '<' => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_page_references() {
        let root = std::env::temp_dir().join(format!("dumb_web_{}", std::process::id()));
        std::fs::create_dir_all(root.join("Assets/UI")).unwrap();
        std::fs::write(root.join("Assets/UI/menu.html"), "<h1>hi</h1>").unwrap();
        assert_eq!(resolve("https://example.com/a", &root), Source::Url("https://example.com/a".into()));
        assert_eq!(resolve("www.rust-lang.org", &root), Source::Url("https://www.rust-lang.org".into()));
        assert_eq!(resolve("  <b>x</b>", &root), Source::Html("<b>x</b>".into()));
        match resolve("UI/menu.html", &root) {
            Source::Url(u) => assert!(u.starts_with("file:///") && u.ends_with("Assets/UI/menu.html"), "{u}"),
            s => panic!("{s:?}"),
        }
        assert_eq!(serde_json_string("a\"b</script>"), "\"a\\\"b\\u003c/script>\"");
        let _ = std::fs::remove_dir_all(root);
    }
}
