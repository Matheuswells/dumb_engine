//! Loading, hot-reloading and building script libraries.

use crate::project::{self, BuildMessage};
use crate::{Registry, ScriptContext, SystemDesc};
use std::collections::{HashMap, HashSet};
use dumb_ecs::World;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Instant, SystemTime};

#[derive(Clone, Debug, PartialEq)]
pub enum ScriptStatus {
    NotLoaded,
    Loaded { components: usize, systems: usize },
    Building,
    BuildFailed,
    Error(String),
}

struct Loaded {
    // Field order matters: the registry (holding fn pointers into the library) drops first.
    registry: Registry,
    #[allow(dead_code)] // kept alive while the registry points into it
    lib: libloading::Library,
    shadow: PathBuf,
}

/// Owns the script library and drives its lifecycle.
pub struct ScriptHost {
    /// The library cargo produces (e.g. `target/debug/game_scripts.dll`).
    pub library_path: PathBuf,
    /// Directory for shadow copies.
    pub cache_dir: PathBuf,
    /// Cargo package to build (`cargo build -p <package>`), run in `workspace_dir`.
    pub package: String,
    pub workspace_dir: PathBuf,
    pub release: bool,
    loaded: Option<Loaded>,
    lib_mtime: Option<SystemTime>,
    last_check: Instant,
    pub status: ScriptStatus,
    pub build_log: Vec<String>,
    build: Option<(Child, Receiver<String>)>,
    reload_counter: u32,
    /// Per-system last error, cleared on success.
    pub errors: Vec<(String, String)>,
    pub auto_reload: bool,
    /// Rebuild automatically when a source file in `src/` is saved.
    pub auto_build: bool,
    /// Systems switched off from the editor (by name).
    pub disabled: HashSet<String>,
    /// Smoothed per-system cost in milliseconds.
    pub timings: HashMap<String, f32>,
    /// Errors and warnings with source locations from the last build.
    pub messages: Vec<BuildMessage>,
    src_mtime: Option<SystemTime>,
    last_src_check: Instant,
    build_started: Option<Instant>,
    /// Duration of the last finished build.
    pub last_build_time: Option<std::time::Duration>,
}

/// Script log records arrive here; they are re-emitted with a `script::<module path>` target so
/// the editor console can tell them apart and show which script logged.
fn host_log(level: log::Level, target: &str, msg: &str, file: &str, line: u32) {
    let target = format!("script::{target}");
    log::logger().log(
        &log::Record::builder()
            .level(level)
            .target(&target)
            .file((!file.is_empty()).then_some(file))
            .line((line > 0).then_some(line))
            .args(format_args!("{msg}"))
            .build(),
    );
}

impl ScriptHost {
    pub fn new(workspace_dir: impl Into<PathBuf>, package: &str, cache_dir: impl Into<PathBuf>) -> Self {
        let workspace_dir = workspace_dir.into();
        let file = format!("{}{}{}", std::env::consts::DLL_PREFIX, package, std::env::consts::DLL_SUFFIX);
        let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
        ScriptHost {
            library_path: workspace_dir.join("target").join(profile).join(file),
            cache_dir: cache_dir.into(),
            package: package.into(),
            workspace_dir,
            release: !cfg!(debug_assertions),
            loaded: None,
            lib_mtime: None,
            last_check: Instant::now(),
            status: ScriptStatus::NotLoaded,
            build_log: Vec::new(),
            build: None,
            reload_counter: 0,
            errors: Vec::new(),
            auto_reload: true,
            auto_build: false,
            disabled: HashSet::new(),
            timings: HashMap::new(),
            messages: Vec::new(),
            src_mtime: None,
            last_src_check: Instant::now(),
            build_started: None,
            last_build_time: None,
        }
    }

    /// Whether the scripts crate exists (has a Cargo.toml).
    pub fn has_crate(&self) -> bool {
        self.workspace_dir.join("Cargo.toml").exists()
    }

    /// Directory of the scripts crate (its `src/` holds the scripts).
    pub fn crate_dir(&self) -> &Path {
        &self.workspace_dir
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded.is_some()
    }

    pub fn systems(&self) -> &[SystemDesc] {
        self.loaded.as_ref().map_or(&[], |l| &l.registry.systems)
    }

    /// Descriptors of the loaded plugin components (to register into new worlds).
    pub fn component_descriptors(&self) -> Vec<dumb_ecs::ComponentDescriptor> {
        self.loaded.as_ref().map_or(Vec::new(), |l| l.registry.components.clone())
    }

    pub fn component_names(&self) -> Vec<String> {
        self.loaded.as_ref().map_or(Vec::new(), |l| l.registry.components.iter().map(|c| c.name.clone()).collect())
    }

    /// Unload: extract plugin component data into the worlds' "missing" storage, then drop the library.
    pub fn unload(&mut self, worlds: &mut [&mut World]) {
        let Some(l) = self.loaded.take() else { return };
        for w in worlds.iter_mut() {
            // Event payloads may be types (and drop code) from the library being unloaded.
            w.clear_events();
            for c in &l.registry.components {
                w.unregister(&c.name);
            }
        }
        let shadow = l.shadow.clone();
        drop(l);
        let _ = std::fs::remove_file(&shadow);
        self.status = ScriptStatus::NotLoaded;
    }

    /// (Re)load the library and register its components in every world.
    pub fn load(&mut self, worlds: &mut [&mut World]) -> Result<(), String> {
        if !self.library_path.exists() {
            self.status = ScriptStatus::NotLoaded;
            return Err(format!("{} not built yet", self.library_path.display()));
        }
        self.unload(worlds);
        std::fs::create_dir_all(&self.cache_dir).map_err(|e| e.to_string())?;
        self.reload_counter += 1;
        let ext = self.library_path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let shadow = self.cache_dir.join(format!("{}_{}_{}.{ext}", self.package, std::process::id(), self.reload_counter));
        std::fs::copy(&self.library_path, &shadow).map_err(|e| format!("shadow copy failed: {e}"))?;
        self.lib_mtime = std::fs::metadata(&self.library_path).and_then(|m| m.modified()).ok();

        let result = unsafe { Self::open(&shadow, &self.package) };
        match result {
            Ok((lib, registry)) => {
                for w in worlds.iter_mut() {
                    for c in &registry.components {
                        w.register(c.clone());
                    }
                }
                self.status = ScriptStatus::Loaded { components: registry.components.len(), systems: registry.systems.len() };
                log::info!(
                    "scripts loaded: {} components, {} systems",
                    registry.components.len(),
                    registry.systems.len()
                );
                self.loaded = Some(Loaded { registry, lib, shadow });
                self.errors.clear();
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&shadow);
                self.status = ScriptStatus::Error(e.clone());
                Err(e)
            }
        }
    }

    unsafe fn open(path: &Path, package: &str) -> Result<(libloading::Library, Registry), String> {
        let lib = libloading::Library::new(path).map_err(|e| format!("load failed: {e}"))?;
        let abi: libloading::Symbol<unsafe extern "C" fn(*mut usize) -> *const u8> =
            lib.get(b"dumb_plugin_abi").map_err(|_| "not a Dumb Engine plugin (missing dumb_plugin_abi)".to_string())?;
        let mut len = 0usize;
        let ptr = abi(&mut len);
        let plugin_abi = String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).into_owned();
        let host_abi = crate::abi_string();
        if plugin_abi != host_abi {
            return Err(format!("ABI mismatch: plugin `{plugin_abi}` vs engine `{host_abi}` — rebuild scripts"));
        }
        let register: libloading::Symbol<fn(&mut Registry)> =
            lib.get(b"dumb_plugin_register").map_err(|_| "missing dumb_plugin_register".to_string())?;
        let mut reg = Registry::new(package, host_log);
        register(&mut reg);
        Ok((lib, reg))
    }

    /// Run every system once. Errors (panics) are recorded per system; the game keeps running.
    pub fn run_systems(&mut self, ctx: &mut ScriptContext) {
        let Some(l) = &self.loaded else { return };
        for s in &l.registry.systems {
            if self.disabled.contains(&s.name) {
                continue;
            }
            let _scope = dumb_core::profiler::Scope::new(dumb_core::profiler::intern(&s.name));
            let t0 = Instant::now();
            let result = (s.runner)(s.func, ctx);
            let ms = t0.elapsed().as_secs_f32() * 1000.0;
            let avg = self.timings.entry(s.name.clone()).or_insert(ms);
            *avg = *avg * 0.9 + ms * 0.1;
            match result {
                Ok(()) => {}
                Err(e) => {
                    if !self.errors.iter().any(|(n, _)| *n == s.name) {
                        log::error!("script system `{}` panicked: {e}", s.name);
                        self.errors.push((s.name.clone(), e));
                    }
                }
            }
        }
    }

    /// Start `cargo build -p <package>` in the background.
    pub fn start_build(&mut self) {
        if self.build.is_some() {
            return;
        }
        let mut cmd = Command::new("cargo");
        cmd.arg("build").arg("-p").arg(&self.package).arg("--message-format=short");
        if self.release {
            cmd.arg("--release");
        }
        cmd.current_dir(&self.workspace_dir).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        match cmd.spawn() {
            Ok(mut child) => {
                let (tx, rx) = channel();
                if let Some(err) = child.stderr.take() {
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        use std::io::BufRead;
                        for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
                            let _ = tx.send(line);
                        }
                    });
                }
                self.build_log.clear();
                self.messages.clear();
                self.build_started = Some(Instant::now());
                self.build_log.push(format!("$ cargo build -p {}", self.package));
                self.status = ScriptStatus::Building;
                self.build = Some((child, rx));
            }
            Err(e) => self.status = ScriptStatus::Error(format!("could not run cargo: {e}")),
        }
    }

    pub fn is_building(&self) -> bool {
        self.build.is_some()
    }

    /// Poll the build and the library file. Reloads when the library changed.
    /// Returns true if a reload happened.
    pub fn update(&mut self, worlds: &mut [&mut World]) -> bool {
        let mut build_finished_ok = false;
        if let Some((child, rx)) = &mut self.build {
            while let Ok(line) = rx.try_recv() {
                self.build_log.push(line);
            }
            if let Ok(Some(status)) = child.try_wait() {
                while let Ok(line) = rx.try_recv() {
                    self.build_log.push(line);
                }
                self.build = None;
                self.last_build_time = self.build_started.take().map(|t| t.elapsed());
                self.messages = project::parse_build_messages(&self.build_log, &self.workspace_dir);
                if status.success() {
                    self.build_log.push("build succeeded".into());
                    build_finished_ok = true;
                } else {
                    self.build_log.push("build FAILED".into());
                    self.status = ScriptStatus::BuildFailed;
                }
            }
        }
        if build_finished_ok {
            return self.load(worlds).is_ok();
        }
        if self.auto_build && self.build.is_none() && self.last_src_check.elapsed().as_millis() > 700 {
            self.last_src_check = Instant::now();
            let newest = self.newest_source();
            if self.src_mtime.is_none() {
                self.src_mtime = newest;
            } else if newest.is_some() && newest != self.src_mtime {
                self.src_mtime = newest;
                log::info!("script source changed, rebuilding");
                self.start_build();
            }
        }
        if self.auto_reload && self.build.is_none() && self.last_check.elapsed().as_millis() > 500 {
            self.last_check = Instant::now();
            let mtime = std::fs::metadata(&self.library_path).and_then(|m| m.modified()).ok();
            if mtime.is_some() && mtime != self.lib_mtime {
                // Let the linker finish writing.
                std::thread::sleep(std::time::Duration::from_millis(150));
                return self.load(worlds).is_ok();
            }
        }
        false
    }
}

impl Drop for ScriptHost {
    fn drop(&mut self) {
        if let Some((mut child, _)) = self.build.take() {
            let _ = child.kill();
        }
        if let Some(l) = self.loaded.take() {
            let shadow = l.shadow.clone();
            // Worlds must already have unregistered (ScriptHost::unload) before the host drops.
            drop(l);
            let _ = std::fs::remove_file(shadow);
        }
    }
}

impl ScriptHost {
    /// Newest modification time of the crate's sources (src/, build.rs, Cargo.toml).
    fn newest_source(&self) -> Option<SystemTime> {
        let mut newest = None;
        let mut consider = |p: &Path| {
            if let Ok(t) = std::fs::metadata(p).and_then(|m| m.modified()) {
                if newest.is_none_or(|n| t > n) {
                    newest = Some(t);
                }
            }
        };
        consider(&self.workspace_dir.join("Cargo.toml"));
        consider(&self.workspace_dir.join("build.rs"));
        let mut stack = vec![self.workspace_dir.join("src")];
        while let Some(dir) = stack.pop() {
            consider(&dir);
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    consider(&p);
                }
            }
        }
        newest
    }

    /// Forget the crate's build artifacts, then build from scratch.
    pub fn clean_build(&mut self) {
        if self.build.is_some() {
            return;
        }
        let mut cmd = Command::new("cargo");
        cmd.arg("clean").arg("-p").arg(&self.package).current_dir(&self.workspace_dir);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let _ = cmd.output();
        self.start_build();
    }

    /// Mark the current sources as seen (call after writing a file that is built right away).
    pub fn mark_sources_seen(&mut self) {
        self.src_mtime = self.newest_source();
    }

    /// Smoothed cost of all script systems in milliseconds.
    pub fn total_time_ms(&self) -> f32 {
        self.systems().iter().filter_map(|s| self.timings.get(&s.name)).sum()
    }
}
