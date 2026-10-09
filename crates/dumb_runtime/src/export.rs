//! Build & Export: package a project as a standalone game folder.
//!
//! ```text
//! <out>/
//!   <Game>.exe            the player (dumb-player), renamed
//!   game_scripts.dll      the project's scripts, if any
//!   project.ron           settings (startup scene, window, physics...)
//!   Assets/...            assets + .meta files (ids)
//!   Library/Imported/     Blender conversions, so players don't need Blender
//!   .dumb-build           marks the folder as an export (safe to overwrite)
//! ```

use crate::{engine_root, ProjectSettings};
use dumb_asset::{AssetDatabase, AssetKind, LoadState};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Marker file written into every export folder.
pub const MARKER: &str = ".dumb-build";

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub project_root: PathBuf,
    pub out_dir: PathBuf,
    pub game_name: String,
    /// Optimized build (slower to compile, much faster game).
    pub release: bool,
    pub include_scripts: bool,
    /// Overrides `startup_scene` (relative to `Assets/`).
    pub startup_scene: String,
    /// Skip compiling and reuse the player/scripts already built (tests, quick re-exports).
    pub skip_compile: bool,
}

#[derive(Clone, Debug)]
pub enum ExportEvent {
    /// A new step started: (title, overall progress 0..1).
    Step(String, f32),
    Log(String),
    Warning(String),
}

/// File name of a scripts library: `game_scripts.dll`, `libgame_scripts.so`...
pub fn library_file(package: &str) -> String {
    format!("{}{}{}", std::env::consts::DLL_PREFIX, package, std::env::consts::DLL_SUFFIX)
}

fn exe_name(name: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect();
    let clean = clean.trim();
    format!("{}{}", if clean.is_empty() { "Game" } else { clean }, std::env::consts::EXE_SUFFIX)
}

/// Run the whole export. Blocking; call from a worker thread.
pub fn export(opts: &ExportOptions, cancel: &AtomicBool, on: &mut dyn FnMut(ExportEvent)) -> Result<PathBuf, String> {
    let t0 = Instant::now();
    let settings: ProjectSettings = std::fs::read_to_string(opts.project_root.join("project.ron"))
        .ok()
        .and_then(|s| ron::from_str(&s).ok())
        .ok_or("project.ron missing or invalid")?;
    let profile = if opts.release { "release" } else { "debug" };
    let check = |cancel: &AtomicBool| if cancel.load(Ordering::Relaxed) { Err("cancelled".to_string()) } else { Ok(()) };

    // 1. Validate.
    on(ExportEvent::Step("Checking project".into(), 0.0));
    let scene = if opts.startup_scene.is_empty() { settings.startup_scene.clone() } else { opts.startup_scene.clone() };
    if scene.is_empty() || !opts.project_root.join("Assets").join(&scene).exists() {
        return Err(format!("startup scene `{scene}` not found in Assets/"));
    }
    prepare_out_dir(&opts.out_dir, &opts.project_root)?;

    // 2. Player executable.
    let engine = engine_root();
    let player = engine.join("target").join(profile).join(format!("dumb-player{}", std::env::consts::EXE_SUFFIX));
    if !opts.skip_compile {
        on(ExportEvent::Step(format!("Compiling player ({profile})"), 0.05));
        let mut args = vec!["build", "-p", "dumb_runtime", "--bin", "dumb-player", "--message-format=short"];
        if opts.release {
            args.push("--release");
        }
        cargo(&engine, &args, cancel, on)?;
    }
    if !player.exists() {
        return Err(format!("player not built: {}", player.display()));
    }
    check(cancel)?;

    // 3. Scripts.
    let scripts_dir = opts.project_root.join(&settings.scripts_workspace);
    let lib = scripts_dir.join("target").join(profile).join(library_file(&settings.scripts_package));
    let with_scripts = opts.include_scripts && scripts_dir.join("Cargo.toml").exists();
    if with_scripts {
        if !opts.skip_compile {
            on(ExportEvent::Step(format!("Compiling scripts ({profile})"), 0.35));
            let mut args = vec!["build", "-p", settings.scripts_package.as_str(), "--message-format=short"];
            if opts.release {
                args.push("--release");
            }
            cargo(&scripts_dir, &args, cancel, on)?;
        }
        if !lib.exists() {
            return Err(format!("scripts library not built: {}", lib.display()));
        }
    }
    check(cancel)?;

    // 4. Make sure every Blender-converted model has an up-to-date cache.
    on(ExportEvent::Step("Converting models".into(), 0.6));
    convert_models(&opts.project_root, cancel, on)?;

    // 5. Copy.
    on(ExportEvent::Step("Copying files".into(), 0.8));
    let out = &opts.out_dir;
    let exe = out.join(exe_name(&opts.game_name));
    std::fs::copy(&player, &exe).map_err(|e| format!("copy player: {e}"))?;
    if with_scripts {
        std::fs::copy(&lib, out.join(library_file(&settings.scripts_package))).map_err(|e| format!("copy scripts: {e}"))?;
    }
    let mut files = 0usize;
    let mut bytes = 0u64;
    copy_assets(&opts.project_root.join("Assets"), &out.join("Assets"), &mut files, &mut bytes)?;
    let imported = opts.project_root.join("Library").join("Imported");
    if imported.exists() {
        let dst = out.join("Library").join("Imported");
        std::fs::create_dir_all(&dst).map_err(|e| e.to_string())?;
        for e in std::fs::read_dir(&imported).map_err(|e| e.to_string())?.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "glb") {
                bytes += std::fs::copy(&p, dst.join(e.file_name())).map_err(|e| e.to_string())?;
                files += 1;
            }
        }
    }
    let mut s = settings.clone();
    s.name = opts.game_name.clone();
    s.startup_scene = scene;
    let ron = ron::ser::to_string_pretty(&s, ron::ser::PrettyConfig::default()).map_err(|e| e.to_string())?;
    std::fs::write(out.join("project.ron"), ron).map_err(|e| e.to_string())?;
    std::fs::write(out.join(MARKER), format!("exported from {}\nprofile {profile}\n", opts.project_root.display())).map_err(|e| e.to_string())?;
    bytes += std::fs::metadata(&exe).map(|m| m.len()).unwrap_or(0);

    on(ExportEvent::Step("Done".into(), 1.0));
    on(ExportEvent::Log(format!(
        "exported {} files, {:.1} MB, in {:.1}s → {}",
        files + 2,
        bytes as f64 / 1_048_576.0,
        t0.elapsed().as_secs_f32(),
        exe.display()
    )));
    Ok(exe)
}

/// Create the output folder; only wipe it when it's empty or a previous export.
fn prepare_out_dir(out: &Path, project: &Path) -> Result<(), String> {
    let canon = |p: &Path| crate::strip_unc(&p.canonicalize().unwrap_or(p.to_path_buf()));
    if out.exists() {
        let o = canon(out);
        let p = canon(project);
        if p.starts_with(&o) || o == p || (o.starts_with(&p) && !o.starts_with(p.join("Builds"))) {
            return Err("choose an output folder outside the project (or under its Builds/ folder)".into());
        }
        let empty = std::fs::read_dir(out).map(|mut d| d.next().is_none()).unwrap_or(true);
        if !empty {
            if !out.join(MARKER).exists() {
                return Err(format!("{} is not empty and is not a previous export; pick an empty folder", out.display()));
            }
            std::fs::remove_dir_all(out).map_err(|e| format!("cleaning {}: {e} (is the game still running?)", out.display()))?;
        }
    }
    std::fs::create_dir_all(out).map_err(|e| e.to_string())
}

fn copy_assets(src: &Path, dst: &Path, files: &mut usize, bytes: &mut u64) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        let p = e.path();
        let is_meta = p.extension().is_some_and(|x| x == "meta");
        if p.is_dir() {
            if !e.file_name().to_string_lossy().starts_with('.') {
                copy_assets(&p, &dst.join(e.file_name()), files, bytes)?;
            }
        } else if is_meta || !dumb_asset::database::is_ignored(&p) {
            *bytes += std::fs::copy(&p, dst.join(e.file_name())).map_err(|err| format!("copy {}: {err}", p.display()))?;
            *files += 1;
        }
    }
    Ok(())
}

fn convert_models(root: &Path, cancel: &AtomicBool, on: &mut dyn FnMut(ExportEvent)) -> Result<(), String> {
    let mut db = AssetDatabase::open(root).map_err(|e| e.to_string())?;
    let models: Vec<_> = db
        .entries()
        .filter(|e| e.kind == AssetKind::Model)
        .filter(|e| !matches!(e.path.rsplit('.').next().map(|x| x.to_ascii_lowercase()).as_deref(), Some("gltf" | "glb")))
        .map(|e| (e.id, e.path.clone()))
        .collect();
    if models.is_empty() {
        return Ok(());
    }
    if db.blender.is_none() {
        on(ExportEvent::Warning("Blender not found: using existing conversions as they are".into()));
    }
    for (id, _) in &models {
        db.load(*id);
    }
    let start = Instant::now();
    while db.is_busy() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if start.elapsed() > Duration::from_secs(900) {
            return Err("model conversion timed out".into());
        }
        db.update();
        std::thread::sleep(Duration::from_millis(30));
    }
    db.update();
    for (id, path) in &models {
        match db.entry(*id).map(|e| e.state.clone()) {
            Some(LoadState::Failed(err)) => on(ExportEvent::Warning(format!("{path}: {err}"))),
            _ => on(ExportEvent::Log(format!("model ok: {path}"))),
        }
    }
    Ok(())
}

/// Run cargo, forwarding its output line by line.
fn cargo(dir: &Path, args: &[&str], cancel: &AtomicBool, on: &mut dyn FnMut(ExportEvent)) -> Result<(), String> {
    on(ExportEvent::Log(format!("> cargo {}  (in {})", args.join(" "), dir.display())));
    let mut cmd = Command::new("cargo");
    cmd.args(args).current_dir(dir).stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // no console window
    }
    let mut child = cmd.spawn().map_err(|e| format!("cargo not found: {e}"))?;
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    loop {
        while let Ok(line) = rx.try_recv() {
            let t = line.trim_start();
            if t.starts_with("error") {
                on(ExportEvent::Warning(line));
            } else {
                on(ExportEvent::Log(line));
            }
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            return Err("cancelled".into());
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            while let Ok(line) = rx.recv_timeout(Duration::from_millis(100)) {
                on(ExportEvent::Log(line));
            }
            return if status.success() { Ok(()) } else { Err(format!("cargo {} failed ({status})", args[0])) };
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_wipe_foreign_folders() {
        let base = std::env::temp_dir().join(format!("dumb_export_{}", std::process::id()));
        let proj = base.join("Proj");
        let out = base.join("Out");
        std::fs::create_dir_all(proj.join("Assets")).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("precious.txt"), "x").unwrap();
        assert!(prepare_out_dir(&out, &proj).is_err());
        assert!(out.join("precious.txt").exists());
        assert!(prepare_out_dir(&proj.join("Assets"), &proj).is_err(), "inside the project");
        std::fs::write(out.join(MARKER), "").unwrap();
        assert!(prepare_out_dir(&out, &proj).is_ok());
        assert!(!out.join("precious.txt").exists());
        assert!(prepare_out_dir(&proj.join("Builds/Win"), &proj).is_ok());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn copies_assets_with_meta_but_not_backups() {
        let base = std::env::temp_dir().join(format!("dumb_export_copy_{}", std::process::id()));
        let src = base.join("Assets");
        std::fs::create_dir_all(src.join("Models")).unwrap();
        for f in ["Models/a.blend", "Models/a.blend.meta", "Models/a.blend1", "x.tmp", "Main.scene", "Main.scene.meta"] {
            std::fs::write(src.join(f), "x").unwrap();
        }
        let (mut n, mut b) = (0, 0);
        copy_assets(&src, &base.join("Out"), &mut n, &mut b).unwrap();
        let out = base.join("Out");
        assert!(out.join("Models/a.blend").exists() && out.join("Models/a.blend.meta").exists() && out.join("Main.scene.meta").exists());
        assert!(!out.join("Models/a.blend1").exists() && !out.join("x.tmp").exists());
        assert_eq!(n, 4);
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod e2e {
    use super::*;

    /// Exports the demo project with the already-built debug player and scripts.
    /// `cargo test -p dumb_runtime export_demo -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn export_demo() {
        let out = std::env::var("DUMB_EXPORT_OUT").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir().join("dumb_export_demo"));
        let opts = ExportOptions {
            project_root: engine_root().join("project"),
            out_dir: out.clone(),
            game_name: "Dumb Demo".into(),
            release: false,
            include_scripts: true,
            startup_scene: String::new(),
            skip_compile: true,
        };
        let r = export(&opts, &AtomicBool::new(false), &mut |e| println!("{e:?}"));
        let exe = r.expect("export");
        assert!(exe.exists() && out.join("project.ron").exists() && out.join(MARKER).exists());
        assert!(out.join(library_file("game_scripts")).exists());
    }
}
