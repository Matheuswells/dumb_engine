//! The asset database: scans the project's `Assets/` folder, owns `.meta` sidecars, imports
//! assets on background threads, tracks dependencies and hot-reloads on file changes.

use crate::import_blender::{self, BLENDER_FORMATS};
use crate::material::MaterialData;
use crate::model::{AnimationEvent, ModelData};
use crate::primitives;
use crate::texture::TextureData;
use dumb_core::AssetId;
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AssetKind {
    Model,
    Texture,
    Material,
    Scene,
    Prefab,
    Audio,
    Script,
    Shader,
    Other,
}

impl AssetKind {
    pub fn from_path(p: &Path) -> AssetKind {
        let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "gltf" | "glb" => AssetKind::Model,
            e if BLENDER_FORMATS.contains(&e) => AssetKind::Model,
            "png" | "jpg" | "jpeg" | "tga" | "bmp" | "hdr" | "gif" | "webp" => AssetKind::Texture,
            "mat" => AssetKind::Material,
            "scene" => AssetKind::Scene,
            "prefab" => AssetKind::Prefab,
            "wav" | "ogg" | "mp3" | "flac" => AssetKind::Audio,
            "rs" => AssetKind::Script,
            "wgsl" => AssetKind::Shader,
            _ => AssetKind::Other,
        }
    }

    /// Name used by `#[editor(asset = "...")]` pickers.
    pub fn tag(&self) -> &'static str {
        match self {
            AssetKind::Model => "model",
            AssetKind::Texture => "texture",
            AssetKind::Material => "material",
            AssetKind::Scene => "scene",
            AssetKind::Prefab => "prefab",
            AssetKind::Audio => "audio",
            AssetKind::Script => "script",
            AssetKind::Shader => "shader",
            AssetKind::Other => "other",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            AssetKind::Model => "🔷",
            AssetKind::Texture => "🖼",
            AssetKind::Material => "🎨",
            AssetKind::Scene => "🌍",
            AssetKind::Prefab => "📦",
            AssetKind::Audio => "🔊",
            AssetKind::Script => "📜",
            AssetKind::Shader => "✨",
            AssetKind::Other => "📄",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Pivot {
    /// Keep the origin from the source file.
    #[default]
    Keep,
    /// Origin at the center of the bounds.
    Center,
    /// Origin at the bottom center (good for props and characters).
    BottomCenter,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum NormalsMode {
    /// Use the normals stored in the file.
    #[default]
    Import,
    /// Recalculate smooth normals (vertices at the same position share a normal).
    Smooth,
    /// Faceted look: one normal per triangle.
    Flat,
}

/// Level-of-detail generation and switching.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LodSettings {
    /// Generate simplified levels at import (meshoptimizer).
    pub generate: bool,
    /// Triangle ratio of each generated level, e.g. [0.5, 0.25, 0.1].
    pub ratios: Vec<f32>,
    /// Allowed simplification error, relative to the mesh size.
    pub max_error: f32,
    /// Screen-height fraction above which each level is used (LOD0 first).
    pub screen_sizes: Vec<f32>,
    /// Hide the object entirely below this screen size (0 = never).
    pub cull_screen_size: f32,
}

impl Default for LodSettings {
    fn default() -> Self {
        LodSettings { generate: false, ratios: vec![0.5, 0.25, 0.1], max_error: 0.05, screen_sizes: vec![0.25, 0.1, 0.04, 0.0], cull_screen_size: 0.0 }
    }
}

/// Non-destructive model edits, applied every time the model is imported.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ImportSettings {
    /// Uniform scale applied to imported geometry.
    pub scale: f32,
    /// Extra rotation in degrees (e.g. -90 on X to fix Z-up exports).
    pub rotation: [f32; 3],
    pub pivot: Pivot,
    pub normals: NormalsMode,
    pub lod: LodSettings,
    /// Material slot -> material asset replacing the imported material.
    pub material_remap: Vec<(usize, AssetId)>,
    /// Nodes (by name) that are not rendered.
    pub hidden_nodes: Vec<String>,
    /// Extra nodes (by name) treated as collision proxies.
    pub collision_nodes: Vec<String>,
}

impl Default for ImportSettings {
    fn default() -> Self {
        ImportSettings {
            scale: 1.0,
            rotation: [0.0; 3],
            pivot: Pivot::Keep,
            normals: NormalsMode::Import,
            lod: LodSettings::default(),
            material_remap: Vec::new(),
            hidden_nodes: Vec::new(),
            collision_nodes: Vec::new(),
        }
    }
}

/// Contents of a `.meta` sidecar file.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AssetMeta {
    pub id: AssetId,
    #[serde(default)]
    pub import: ImportSettings,
    /// Load as soon as the project opens.
    #[serde(default)]
    pub preload: bool,
    #[serde(default)]
    pub labels: Vec<String>,
    /// Per-clip animation events, edited in the animation viewer.
    #[serde(default)]
    pub animation_events: Vec<(String, Vec<AnimationEvent>)>,
}

impl AssetMeta {
    fn new() -> Self {
        AssetMeta {
            id: AssetId::new(),
            import: ImportSettings::default(),
            preload: false,
            labels: Vec::new(),
            animation_events: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LoadState {
    Unloaded,
    Loading,
    Loaded,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct AssetEntry {
    pub id: AssetId,
    /// Path relative to `Assets/`, with `/` separators.
    pub path: String,
    pub kind: AssetKind,
    pub meta: AssetMeta,
    /// Bumped on every (re)import. Renderers compare it to know when to re-upload.
    pub version: u64,
    pub state: LoadState,
    /// Assets this one depends on.
    pub deps: Vec<AssetId>,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub import_time: Option<Duration>,
}

impl AssetEntry {
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    pub fn stem(&self) -> &str {
        let n = self.name();
        n.rsplit_once('.').map_or(n, |(s, _)| s)
    }

    pub fn folder(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(f, _)| f)
    }
}

#[derive(Clone, Debug)]
pub enum AssetEvent {
    Added(AssetId),
    Imported(AssetId),
    Reimported(AssetId),
    Failed(AssetId, String),
    Removed(AssetId),
    Moved(AssetId),
}

/// Where a sub-asset (e.g. a texture embedded in a model) comes from.
#[derive(Clone, Copy, Debug)]
pub struct SubAsset {
    pub parent: AssetId,
    pub kind: AssetKind,
    pub index: usize,
}

enum JobResult {
    Model(AssetId, u64, Result<ModelData, String>, Duration),
    Texture(AssetId, u64, Result<TextureData, String>, Duration),
}

pub struct AssetDatabase {
    pub project_root: PathBuf,
    pub assets_root: PathBuf,
    pub library_root: PathBuf,
    entries: HashMap<AssetId, AssetEntry>,
    by_path: HashMap<String, AssetId>,
    sub_assets: HashMap<AssetId, SubAsset>,
    models: HashMap<AssetId, Arc<ModelData>>,
    textures: HashMap<AssetId, Arc<TextureData>>,
    materials: HashMap<AssetId, MaterialData>,
    job_tx: Sender<JobResult>,
    job_rx: Receiver<JobResult>,
    _watcher: Option<notify::RecommendedWatcher>,
    fs_rx: Option<Receiver<notify::Result<notify::Event>>>,
    pending_fs: HashMap<PathBuf, Instant>,
    /// Paths written by the database itself; their next change event is ignored.
    self_writes: HashSet<PathBuf>,
    events: Vec<AssetEvent>,
    pub blender: Option<PathBuf>,
    version_counter: u64,
}

fn rel_key(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").trim_start_matches('/').to_string()
}

/// Files the database never treats as assets: sidecars, editor backups, temp files.
pub fn is_ignored(p: &Path) -> bool {
    let name = p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if name.starts_with('.') || name.starts_with('~') || name.ends_with('~') {
        return true;
    }
    let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    // Blender keeps numbered backups next to the .blend (Robot.blend1, Robot.blend2, ...).
    let blender_backup = ext.len() > 5 && ext.starts_with("blend") && ext[5..].chars().all(|c| c.is_ascii_digit());
    blender_backup || matches!(ext.as_str(), "meta" | "tmp" | "swp" | "bak" | "crdownload")
}

fn meta_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".meta");
    PathBuf::from(s)
}

impl AssetDatabase {
    /// Open a project directory (the folder that contains `Assets/`).
    pub fn open(project_root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let project_root: PathBuf = project_root.into();
        let assets_root = project_root.join("Assets");
        let library_root = project_root.join("Library");
        std::fs::create_dir_all(&assets_root)?;
        std::fs::create_dir_all(&library_root)?;
        let (job_tx, job_rx) = channel();

        let (fs_tx, fs_rx) = channel();
        let watcher = match notify::recommended_watcher(move |res| {
            let _ = fs_tx.send(res);
        }) {
            Ok(mut w) => match w.watch(&assets_root, RecursiveMode::Recursive) {
                Ok(()) => Some(w),
                Err(e) => {
                    log::warn!("asset watcher disabled: {e}");
                    None
                }
            },
            Err(e) => {
                log::warn!("asset watcher disabled: {e}");
                None
            }
        };

        let blender = import_blender::find_blender();
        match &blender {
            Some(b) => log::info!("Blender found at {}", b.display()),
            None => log::warn!("Blender not found; .blend/.fbx/.obj import disabled (set DUMB_BLENDER)"),
        }

        let mut db = AssetDatabase {
            project_root,
            assets_root,
            library_root,
            entries: HashMap::new(),
            by_path: HashMap::new(),
            sub_assets: HashMap::new(),
            models: HashMap::new(),
            textures: HashMap::new(),
            materials: HashMap::new(),
            job_tx,
            job_rx,
            fs_rx: watcher.as_ref().map(|_| fs_rx),
            _watcher: watcher,
            pending_fs: HashMap::new(),
            self_writes: HashSet::new(),
            events: Vec::new(),
            blender,
            version_counter: 1,
        };
        db.scan();
        let preload: Vec<AssetId> = db.entries.values().filter(|e| e.meta.preload).map(|e| e.id).collect();
        for id in preload {
            db.load(id);
        }
        Ok(db)
    }

    // ------------------------------------------------------------------ scanning

    /// Walk `Assets/` and register every file. Creates missing `.meta` files.
    pub fn scan(&mut self) {
        let mut stack = vec![self.assets_root.clone()];
        let mut seen = HashSet::new();
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if is_ignored(&p) {
                    continue;
                } else if let Some(id) = self.register_file(&p) {
                    seen.insert(id);
                }
            }
        }
        let gone: Vec<AssetId> = self.entries.keys().filter(|id| !seen.contains(id)).copied().collect();
        for id in gone {
            self.remove_entry(id);
        }
        self.rebuild_deps();
    }

    fn register_file(&mut self, abs: &Path) -> Option<AssetId> {
        let rel = rel_key(abs.strip_prefix(&self.assets_root).ok()?);
        if let Some(id) = self.by_path.get(&rel) {
            return Some(*id);
        }
        let mp = meta_path(abs);
        let meta = std::fs::read_to_string(&mp)
            .ok()
            .and_then(|s| ron::from_str::<AssetMeta>(&s).ok())
            .unwrap_or_else(|| {
                let m = AssetMeta::new();
                self.write_meta_file(&mp, &m);
                m
            });
        let mut meta = meta;
        // Duplicated files (copy-paste in Explorer) share an id; give the copy a new one.
        if self.entries.contains_key(&meta.id) {
            meta.id = AssetId::new();
            self.write_meta_file(&mp, &meta);
        }
        let md = std::fs::metadata(abs).ok();
        let id = meta.id;
        let entry = AssetEntry {
            id,
            path: rel.clone(),
            kind: AssetKind::from_path(abs),
            meta,
            version: 0,
            state: LoadState::Unloaded,
            deps: Vec::new(),
            size: md.as_ref().map_or(0, |m| m.len()),
            modified: md.and_then(|m| m.modified().ok()),
            import_time: None,
        };
        self.entries.insert(id, entry);
        self.by_path.insert(rel, id);
        self.events.push(AssetEvent::Added(id));
        Some(id)
    }

    fn write_meta_file(&mut self, path: &Path, meta: &AssetMeta) {
        if let Ok(s) = ron::ser::to_string_pretty(meta, ron::ser::PrettyConfig::default()) {
            if let Err(e) = std::fs::write(path, s) {
                log::warn!("could not write {}: {e}", path.display());
            }
        }
    }

    fn remove_entry(&mut self, id: AssetId) {
        if let Some(e) = self.entries.remove(&id) {
            self.by_path.remove(&e.path);
        }
        self.models.remove(&id);
        self.textures.remove(&id);
        self.materials.remove(&id);
        self.sub_assets.retain(|_, s| s.parent != id);
        self.events.push(AssetEvent::Removed(id));
    }

    /// Recompute dependency lists for assets whose references are cheap to read.
    fn rebuild_deps(&mut self) {
        let ids: Vec<AssetId> = self.entries.keys().copied().collect();
        for id in ids {
            let deps = self.compute_deps(id);
            if let Some(e) = self.entries.get_mut(&id) {
                e.deps = deps;
            }
        }
    }

    fn compute_deps(&self, id: AssetId) -> Vec<AssetId> {
        let Some(e) = self.entries.get(&id) else { return Vec::new() };
        let path = self.assets_root.join(&e.path);
        match e.kind {
            AssetKind::Material => std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| MaterialData::from_ron(&s).ok())
                .map(|m| m.textures().into_iter().filter(|t| !t.is_none()).map(|t| self.resolve_parent(t)).collect())
                .unwrap_or_default(),
            AssetKind::Scene | AssetKind::Prefab => std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| dumb_ecs::SceneData::from_ron(&s).ok())
                .map(|s| s.asset_refs().into_iter().map(|a| self.resolve_parent(a)).collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Sub-assets map to their owning file asset.
    pub fn resolve_parent(&self, id: AssetId) -> AssetId {
        self.sub_assets.get(&id).map_or(id, |s| s.parent)
    }

    // ------------------------------------------------------------------ queries

    pub fn entry(&self, id: AssetId) -> Option<&AssetEntry> {
        self.entries.get(&id)
    }

    pub fn entries(&self) -> impl Iterator<Item = &AssetEntry> {
        self.entries.values()
    }

    pub fn id_for_path(&self, rel: &str) -> Option<AssetId> {
        self.by_path.get(rel).copied()
    }

    pub fn abs_path(&self, id: AssetId) -> Option<PathBuf> {
        self.entries.get(&id).map(|e| self.assets_root.join(&e.path))
    }

    pub fn sub_asset(&self, id: AssetId) -> Option<SubAsset> {
        self.sub_assets.get(&id).copied()
    }

    /// Human-readable name for any asset id, including built-ins and sub-assets.
    pub fn display_name(&self, id: AssetId) -> String {
        if id.is_none() {
            return "None".into();
        }
        if let Some((_, n)) = primitives::BUILTIN_MODELS.iter().find(|(b, _)| *b == id) {
            return format!("{n} (built-in)");
        }
        if let Some(e) = self.entries.get(&id) {
            return e.stem().to_string();
        }
        if let Some(s) = self.sub_assets.get(&id) {
            let parent = self.entries.get(&s.parent).map_or("?", |e| e.stem());
            let name = self.models.get(&s.parent).and_then(|m| match s.kind {
                AssetKind::Texture => m.textures.get(s.index).map(|t| t.name.clone()),
                AssetKind::Material => m.material_names.get(s.index).cloned(),
                _ => None,
            });
            return format!("{parent}/{}", name.unwrap_or_else(|| format!("#{}", s.index)));
        }
        format!("Missing {}", &id.0.to_string()[..8])
    }

    /// Asset version; sub-assets report their parent's version.
    pub fn version(&self, id: AssetId) -> u64 {
        let id = self.resolve_parent(id);
        self.entries.get(&id).map_or(0, |e| e.version)
    }

    /// Search by name. `t:model` / `t:texture` / ... filters by kind, `l:label` by label.
    pub fn search(&self, query: &str) -> Vec<AssetId> {
        let mut kind = None;
        let mut label = None;
        let mut words = Vec::new();
        for w in query.split_whitespace() {
            if let Some(k) = w.strip_prefix("t:") {
                kind = Some(k.to_ascii_lowercase());
            } else if let Some(l) = w.strip_prefix("l:") {
                label = Some(l.to_ascii_lowercase());
            } else {
                words.push(w.to_ascii_lowercase());
            }
        }
        let mut out: Vec<&AssetEntry> = self
            .entries
            .values()
            .filter(|e| kind.as_deref().is_none_or(|k| e.kind.tag() == k))
            .filter(|e| label.as_deref().is_none_or(|l| e.meta.labels.iter().any(|x| x.to_ascii_lowercase() == l)))
            .filter(|e| {
                let p = e.path.to_ascii_lowercase();
                words.iter().all(|w| p.contains(w.as_str()))
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out.into_iter().map(|e| e.id).collect()
    }

    /// Assets of a kind (for pickers), plus sub-assets of loaded models.
    pub fn assets_of_kind(&self, tag: &str) -> Vec<AssetId> {
        let mut out: Vec<AssetId> = self.search(&format!("t:{tag}"));
        if tag == "model" {
            out.splice(0..0, primitives::BUILTIN_MODELS.iter().map(|(id, _)| *id));
        }
        for (id, s) in &self.sub_assets {
            if s.kind.tag() == tag {
                out.push(*id);
            }
        }
        out
    }

    pub fn dependencies(&self, id: AssetId) -> &[AssetId] {
        self.entries.get(&id).map_or(&[], |e| &e.deps)
    }

    /// Assets that depend on `id`.
    pub fn references(&self, id: AssetId) -> Vec<AssetId> {
        self.entries.values().filter(|e| e.deps.contains(&id)).map(|e| e.id).collect()
    }

    /// Sub-folders of `folder` ("" = Assets root), relative paths.
    pub fn folders(&self, folder: &str) -> Vec<String> {
        let dir = self.assets_root.join(folder);
        let mut out: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if folder.is_empty() { name } else { format!("{folder}/{name}") }
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }

    /// Assets directly inside `folder`.
    pub fn assets_in(&self, folder: &str) -> Vec<AssetId> {
        let mut out: Vec<&AssetEntry> = self.entries.values().filter(|e| e.folder() == folder).collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out.into_iter().map(|e| e.id).collect()
    }

    // ------------------------------------------------------------------ loading

    fn next_version(&mut self) -> u64 {
        self.version_counter += 1;
        self.version_counter
    }

    /// Start loading an asset if it isn't loaded or loading.
    pub fn load(&mut self, id: AssetId) {
        let Some(e) = self.entries.get(&id) else { return };
        if e.state != LoadState::Unloaded {
            return;
        }
        let kind = e.kind;
        let path = self.assets_root.join(&e.path);
        let settings = e.meta.import.clone();
        let scale = settings.scale;
        match kind {
            AssetKind::Model => {
                let token = self.next_version();
                self.entries.get_mut(&id).unwrap().state = LoadState::Loading;
                let tx = self.job_tx.clone();
                let blender = self.blender.clone();
                let cache = self.library_root.join("Imported").join(format!("{id}.glb"));
                std::thread::spawn(move || {
                    let t0 = Instant::now();
                    let r = import_model(&path, id, scale, blender.as_deref(), &cache).map(|mut m| {
                        crate::processing::post_process(&mut m, &settings);
                        m
                    });
                    let _ = tx.send(JobResult::Model(id, token, r, t0.elapsed()));
                });
            }
            AssetKind::Texture => {
                let token = self.next_version();
                self.entries.get_mut(&id).unwrap().state = LoadState::Loading;
                let tx = self.job_tx.clone();
                std::thread::spawn(move || {
                    let t0 = Instant::now();
                    let r = TextureData::load(&path);
                    let _ = tx.send(JobResult::Texture(id, token, r, t0.elapsed()));
                });
            }
            AssetKind::Material => {
                let r = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| MaterialData::from_ron(&s));
                let v = self.next_version();
                let e = self.entries.get_mut(&id).unwrap();
                match r {
                    Ok(m) => {
                        e.state = LoadState::Loaded;
                        e.version = v;
                        self.materials.insert(id, m);
                        self.events.push(AssetEvent::Imported(id));
                    }
                    Err(err) => {
                        e.state = LoadState::Failed(err.clone());
                        self.events.push(AssetEvent::Failed(id, err));
                    }
                }
            }
            _ => {
                let v = self.next_version();
                let e = self.entries.get_mut(&id).unwrap();
                e.state = LoadState::Loaded;
                e.version = v;
            }
        }
    }

    /// Force a re-import (keeps the old data visible until the new import finishes).
    pub fn reimport(&mut self, id: AssetId) {
        let id = self.resolve_parent(id);
        if let Some(e) = self.entries.get_mut(&id) {
            if let Ok(md) = std::fs::metadata(self.assets_root.join(&e.path)) {
                e.size = md.len();
                e.modified = md.modified().ok();
            }
            e.state = LoadState::Unloaded;
            if e.kind == AssetKind::Model {
                // Blender-converted cache is stale now.
                let _ = std::fs::remove_file(self.library_root.join("Imported").join(format!("{id}.glb")));
            }
        }
        self.load(id);
        let deps = self.compute_deps(id);
        if let Some(e) = self.entries.get_mut(&id) {
            e.deps = deps;
        }
    }

    /// Loaded model, starting a load if needed. Built-in primitives are always available.
    pub fn model(&mut self, id: AssetId) -> Option<Arc<ModelData>> {
        if let Some(m) = self.models.get(&id) {
            return Some(m.clone());
        }
        if primitives::is_builtin(id) {
            let m = Arc::new(primitives::builtin_model(id)?);
            self.models.insert(id, m.clone());
            return Some(m);
        }
        self.load(id);
        None
    }

    /// Already-loaded model without triggering a load.
    pub fn model_loaded(&self, id: AssetId) -> Option<Arc<ModelData>> {
        self.models.get(&id).cloned()
    }

    pub fn texture(&mut self, id: AssetId) -> Option<Arc<TextureData>> {
        if let Some(t) = self.textures.get(&id) {
            return Some(t.clone());
        }
        if let Some(s) = self.sub_assets.get(&id).copied() {
            let m = self.models.get(&s.parent)?;
            let t = Arc::new(m.textures.get(s.index)?.clone());
            self.textures.insert(id, t.clone());
            return Some(t);
        }
        self.load(id);
        None
    }

    pub fn material(&mut self, id: AssetId) -> Option<MaterialData> {
        if id == primitives::DEFAULT_MATERIAL {
            return Some(MaterialData::default());
        }
        if let Some(m) = self.materials.get(&id) {
            return Some(m.clone());
        }
        if let Some(s) = self.sub_assets.get(&id).copied() {
            return self.models.get(&s.parent)?.materials.get(s.index).cloned();
        }
        self.load(id);
        None
    }

    /// Update a `.mat` asset in memory and on disk (material viewer live editing).
    pub fn set_material(&mut self, id: AssetId, m: MaterialData) -> Result<(), String> {
        let e = self.entries.get(&id).ok_or("not a material asset")?;
        if e.kind != AssetKind::Material {
            return Err("imported materials are read-only; create a .mat to override".into());
        }
        let path = self.assets_root.join(&e.path);
        std::fs::write(&path, m.to_ron()).map_err(|e| e.to_string())?;
        self.self_writes.insert(path);
        self.materials.insert(id, m);
        let v = self.next_version();
        self.entries.get_mut(&id).unwrap().version = v;
        let deps = self.compute_deps(id);
        self.entries.get_mut(&id).unwrap().deps = deps;
        Ok(())
    }

    /// Change a model's import settings, save the `.meta` and re-import it (the Blender-converted
    /// cache is kept: every setting is applied after conversion).
    pub fn set_import_settings(&mut self, id: AssetId, s: ImportSettings) {
        let id = self.resolve_parent(id);
        let Some(e) = self.entries.get_mut(&id) else { return };
        e.meta.import = s;
        e.state = LoadState::Unloaded;
        self.save_meta(id);
        self.load(id);
    }

    pub fn import_settings(&self, id: AssetId) -> Option<&ImportSettings> {
        self.entries.get(&self.resolve_parent(id)).map(|e| &e.meta.import)
    }

    pub fn save_meta(&mut self, id: AssetId) {
        if let Some(e) = self.entries.get(&id) {
            let mp = meta_path(&self.assets_root.join(&e.path));
            let meta = e.meta.clone();
            self.write_meta_file(&mp, &meta);
            self.self_writes.insert(mp);
        }
    }

    pub fn meta_mut(&mut self, id: AssetId) -> Option<&mut AssetMeta> {
        self.entries.get_mut(&id).map(|e| &mut e.meta)
    }

    // ------------------------------------------------------------------ per-frame

    /// Poll background imports and file changes. Call once per frame.
    pub fn update(&mut self) {
        while let Ok(job) = self.job_rx.try_recv() {
            self.finish_job(job);
        }
        if let Some(rx) = &self.fs_rx {
            while let Ok(res) = rx.try_recv() {
                if let Ok(ev) = res {
                    for p in ev.paths {
                        self.pending_fs.insert(p, Instant::now());
                    }
                }
            }
        }
        // Debounce: editors write files in several steps.
        let ready: Vec<PathBuf> = self
            .pending_fs
            .iter()
            .filter(|(_, t)| t.elapsed() > Duration::from_millis(300))
            .map(|(p, _)| p.clone())
            .collect();
        if ready.is_empty() {
            return;
        }
        let mut structure_changed = false;
        for p in ready {
            self.pending_fs.remove(&p);
            if self.self_writes.remove(&p) {
                continue;
            }
            if is_ignored(&p) {
                continue;
            }
            structure_changed |= self.on_file_changed(&p);
        }
        if structure_changed {
            self.scan();
        }
    }

    /// Returns true when a rescan is needed (files added/removed).
    fn on_file_changed(&mut self, abs: &Path) -> bool {
        let Ok(rel) = abs.strip_prefix(&self.assets_root) else { return false };
        let key = rel_key(rel);
        match (abs.is_file(), self.by_path.get(&key).copied()) {
            (true, Some(id)) => {
                log::info!("asset changed on disk: {key}");
                let was_used = self.entries.get(&id).is_some_and(|e| e.state != LoadState::Unloaded);
                if was_used {
                    self.reimport(id);
                }
                false
            }
            _ => true,
        }
    }

    fn finish_job(&mut self, job: JobResult) {
        let (id, token, result, dt) = match job {
            JobResult::Model(id, token, r, dt) => (id, token, r.map(Loaded::Model), dt),
            JobResult::Texture(id, token, r, dt) => (id, token, r.map(Loaded::Texture), dt),
        };
        let Some(e) = self.entries.get(&id) else { return };
        if e.state != LoadState::Loading {
            return;
        }
        let first = e.version == 0;
        match result {
            Ok(loaded) => {
                match loaded {
                    Loaded::Model(m) => {
                        self.sub_assets.retain(|_, s| s.parent != id);
                        for i in 0..m.textures.len() {
                            let sid = id.sub("texture", i);
                            self.sub_assets.insert(sid, SubAsset { parent: id, kind: AssetKind::Texture, index: i });
                            self.textures.remove(&sid);
                        }
                        for i in 0..m.materials.len() {
                            let sid = id.sub("material", i);
                            self.sub_assets.insert(sid, SubAsset { parent: id, kind: AssetKind::Material, index: i });
                        }
                        self.models.insert(id, Arc::new(m));
                    }
                    Loaded::Texture(t) => {
                        self.textures.insert(id, Arc::new(t));
                    }
                }
                let e = self.entries.get_mut(&id).unwrap();
                e.state = LoadState::Loaded;
                e.version = token;
                e.import_time = Some(dt);
                log::info!("imported {} in {:.0} ms", e.path, dt.as_secs_f64() * 1000.0);
                self.events.push(if first { AssetEvent::Imported(id) } else { AssetEvent::Reimported(id) });
            }
            Err(err) => {
                log::error!("import failed for {}: {err}", e.path);
                // Keep the previous data if this was a reimport.
                let e = self.entries.get_mut(&id).unwrap();
                e.state = if first { LoadState::Failed(err.clone()) } else { LoadState::Loaded };
                self.events.push(AssetEvent::Failed(id, err));
            }
        }
    }

    /// Events since the last call (imports, reimports, failures, file changes).
    pub fn drain_events(&mut self) -> Vec<AssetEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn is_busy(&self) -> bool {
        self.entries.values().any(|e| e.state == LoadState::Loading)
    }

    // ------------------------------------------------------------------ file operations

    pub fn create_folder(&mut self, parent: &str, name: &str) -> Result<String, String> {
        let rel = if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") };
        std::fs::create_dir_all(self.assets_root.join(&rel)).map_err(|e| e.to_string())?;
        Ok(rel)
    }

    /// Write a file into the project (new scene, prefab, material) and register it.
    pub fn write_asset(&mut self, rel: &str, contents: &str) -> Result<AssetId, String> {
        let abs = self.assets_root.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&abs, contents).map_err(|e| e.to_string())?;
        self.self_writes.insert(abs.clone());
        let id = self.register_file(&abs).ok_or("path outside Assets")?;
        if let Some(e) = self.entries.get_mut(&id) {
            e.size = contents.len() as u64;
            e.modified = Some(SystemTime::now());
            if e.state != LoadState::Unloaded {
                e.state = LoadState::Unloaded;
            }
        }
        if matches!(self.entries[&id].kind, AssetKind::Material) {
            self.load(id);
        }
        let deps = self.compute_deps(id);
        self.entries.get_mut(&id).unwrap().deps = deps;
        Ok(id)
    }

    pub fn read_asset_text(&self, id: AssetId) -> Option<String> {
        std::fs::read_to_string(self.abs_path(id)?).ok()
    }

    /// Copy an external file into `folder` and register it.
    pub fn import_external(&mut self, src: &Path, folder: &str) -> Result<AssetId, String> {
        let name = src.file_name().ok_or("bad path")?.to_string_lossy().into_owned();
        let mut rel = if folder.is_empty() { name.clone() } else { format!("{folder}/{name}") };
        let mut n = 1;
        while self.assets_root.join(&rel).exists() {
            let (stem, ext) = name.rsplit_once('.').unwrap_or((&name, ""));
            let file = format!("{stem}_{n}.{ext}");
            rel = if folder.is_empty() { file } else { format!("{folder}/{file}") };
            n += 1;
        }
        let dst = self.assets_root.join(&rel);
        std::fs::copy(src, &dst).map_err(|e| e.to_string())?;
        self.self_writes.insert(dst.clone());
        self.register_file(&dst).ok_or_else(|| "register failed".into())
    }

    /// Rename or move a file asset (its `.meta` moves with it, so the id is preserved).
    pub fn move_asset(&mut self, id: AssetId, new_rel: &str) -> Result<(), String> {
        let e = self.entries.get(&id).ok_or("unknown asset")?;
        let old_abs = self.assets_root.join(&e.path);
        let new_abs = self.assets_root.join(new_rel);
        if new_abs.exists() {
            return Err(format!("{new_rel} already exists"));
        }
        if let Some(p) = new_abs.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&old_abs, &new_abs).map_err(|e| e.to_string())?;
        let _ = std::fs::rename(meta_path(&old_abs), meta_path(&new_abs));
        for p in [&old_abs, &new_abs] {
            self.self_writes.insert(p.clone());
            self.self_writes.insert(meta_path(p));
        }
        let old_key = e.path.clone();
        self.by_path.remove(&old_key);
        let key = rel_key(Path::new(new_rel));
        self.by_path.insert(key.clone(), id);
        let e = self.entries.get_mut(&id).unwrap();
        e.path = key;
        e.kind = AssetKind::from_path(&new_abs);
        self.events.push(AssetEvent::Moved(id));
        Ok(())
    }

    pub fn rename_asset(&mut self, id: AssetId, new_name: &str) -> Result<(), String> {
        let folder = self.entries.get(&id).ok_or("unknown asset")?.folder().to_string();
        let rel = if folder.is_empty() { new_name.to_string() } else { format!("{folder}/{new_name}") };
        self.move_asset(id, &rel)
    }

    /// Rename/move a folder; contained assets keep their ids.
    pub fn move_folder(&mut self, old_rel: &str, new_rel: &str) -> Result<(), String> {
        let (a, b) = (self.assets_root.join(old_rel), self.assets_root.join(new_rel));
        if b.exists() {
            return Err(format!("{new_rel} already exists"));
        }
        std::fs::rename(&a, &b).map_err(|e| e.to_string())?;
        let prefix = format!("{old_rel}/");
        let moved: Vec<(AssetId, String)> = self
            .entries
            .values()
            .filter(|e| e.path.starts_with(&prefix))
            .map(|e| (e.id, format!("{new_rel}/{}", &e.path[prefix.len()..])))
            .collect();
        for (id, np) in moved {
            let old = std::mem::replace(&mut self.entries.get_mut(&id).unwrap().path, np.clone());
            self.by_path.remove(&old);
            self.by_path.insert(np, id);
            self.events.push(AssetEvent::Moved(id));
        }
        Ok(())
    }

    /// Move a file or folder into `Library/Trash/<timestamp>/` (recoverable).
    pub fn delete_path(&mut self, rel: &str) -> Result<(), String> {
        let src = self.assets_root.join(rel);
        let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let dst = self.library_root.join("Trash").join(stamp.to_string()).join(rel);
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&src, &dst).map_err(|e| e.to_string())?;
        if src.is_file() || !src.exists() {
            let _ = std::fs::rename(meta_path(&src), meta_path(&dst));
        }
        self.scan();
        Ok(())
    }
}

enum Loaded {
    Model(ModelData),
    Texture(TextureData),
}

fn import_model(path: &Path, id: AssetId, scale: f32, blender: Option<&Path>, cache: &Path) -> Result<ModelData, String> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if ext == "gltf" || ext == "glb" {
        return crate::import_gltf::import(path, id, scale);
    }
    let src_time = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let cache_time = std::fs::metadata(cache).and_then(|m| m.modified()).ok();
    let fresh = matches!((src_time, cache_time), (Some(s), Some(c)) if c >= s);
    // Exported games ship the converted cache and have no Blender: use the cache as-is.
    let usable = fresh || (cache_time.is_some() && blender.is_none());
    if !usable {
        let blender = blender.ok_or("Blender not found: install it or set DUMB_BLENDER to import this format")?;
        import_blender::convert_to_glb(blender, path, cache)?;
    }
    let mut m = crate::import_gltf::import(cache, id, scale)?;
    m.source_format = format!("{} via Blender", ext.to_ascii_uppercase());
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::is_ignored;
    use std::path::Path;

    #[test]
    fn ignores_backups_and_sidecars() {
        for p in ["a/Robot.blend1", "Robot.blend12", "x.png.meta", ".hidden", "~lock.txt", "file.tmp"] {
            assert!(is_ignored(Path::new(p)), "{p}");
        }
        for p in ["Robot.blend", "tex.png", "Main.scene", "Gold.mat"] {
            assert!(!is_ignored(Path::new(p)), "{p}");
        }
    }
}
