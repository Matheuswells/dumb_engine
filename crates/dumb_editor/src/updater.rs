//! Self-update from GitHub releases.
//!
//! Checks the latest release of the repository, downloads the bundle for this platform
//! (`DumbEngine-<tag>-<platform>.zip` / `.tar.gz`, built by `tools/package-release.sh`) and
//! installs it in place: every top-level item of the new bundle replaces the one in the install
//! folder, and the replaced items are kept in `.update/backup` until the next update. The
//! running executable can be renamed on every platform, so nothing waits for the editor to exit.
//! The sample `project/` is never overwritten, and files the bundle doesn't ship (WebView2
//! data, user files) are left alone. Projects keep pointing at the same `crates/` folder, so
//! they build against the new engine after a restart.
//!
//! Only release bundles update themselves. A source checkout updates with `git pull`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const REPO: &str = "Matheuswells/dumb_engine";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const EXE: &str = if cfg!(windows) { ".exe" } else { "" };
/// Work folder inside the install folder (same volume, so installing is a few renames).
const WORK: &str = ".update";

static INSTALL_DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
static RESTART: AtomicBool = AtomicBool::new(false);
/// Shared by every editor instance (one per opened project) and the worker threads.
static STATE: Mutex<State> = Mutex::new(State::Idle);
static STARTUP_CHECKED: AtomicBool = AtomicBool::new(false);

/// Where this editor is installed, or why it can't update itself. Resolved once at startup,
/// because after an update `current_exe()` points into the backup folder.
pub fn install_dir() -> Result<&'static Path, &'static str> {
    INSTALL_DIR
        .get_or_init(|| {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let dir = exe.parent().ok_or("no parent folder")?.to_path_buf();
            if dir.join("crates").join("dumb_script").is_dir() && dir.join("rust-toolchain.toml").is_file() {
                Ok(dumb_runtime::strip_unc(&dir.canonicalize().unwrap_or(dir)))
            } else {
                Err("This editor runs from a source checkout. Update it with `git pull` and rebuild.".into())
            }
        })
        .as_ref()
        .map(PathBuf::as_path)
        .map_err(String::as_str)
}

/// Call once at startup: pins the install folder and removes leftovers of an earlier update.
pub fn startup() {
    if let Ok(dir) = install_dir() {
        let work = dir.join(WORK);
        let _ = std::fs::remove_dir_all(work.join("staging"));
        if let Ok(entries) = std::fs::read_dir(&work) {
            for e in entries.flatten().filter(|e| e.path().is_file()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Start the installed editor again after the event loop has exited, if an update asked for it.
/// `user_toolchain` is `RUSTUP_TOOLCHAIN` as the user started us (before we pinned our own), so
/// the new editor pins the toolchain of the new bundle instead of inheriting the old one.
pub fn restart_if_requested(user_toolchain: Option<std::ffi::OsString>) {
    if !RESTART.load(Ordering::SeqCst) {
        return;
    }
    let Ok(dir) = install_dir() else { return };
    let mut cmd = std::process::Command::new(dir.join(format!("dumb-editor{EXE}")));
    cmd.args(std::env::args_os().skip(1)).current_dir(dir);
    match user_toolchain {
        Some(t) => cmd.env("RUSTUP_TOOLCHAIN", t),
        None => cmd.env_remove("RUSTUP_TOOLCHAIN"),
    };
    if let Err(e) = cmd.spawn() {
        eprintln!("could not restart the editor: {e}");
    }
}

fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("windows-x64"),
        ("linux", "x86_64") => Some("linux-x64"),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct Release {
    pub tag: String,
    pub version: semver::Version,
    pub notes: String,
    pub page: String,
    /// The bundle for this platform, if the release has one.
    pub asset: Option<Asset>,
}

#[derive(Clone, Debug)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default)]
pub enum State {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading { release: Release, done: u64, total: u64 },
    Installing(Release),
    /// Installed; takes effect after a restart.
    Installed(Release),
    Failed(String),
}

impl State {
    fn busy(&self) -> bool {
        matches!(self, State::Checking | State::Downloading { .. } | State::Installing(_))
    }
}

/// The Updates window. The state itself is process-wide ([`STATE`]).
#[derive(Default)]
pub struct Updater {
    pub open: bool,
}

#[derive(Default)]
pub struct UpdaterOutput {
    /// The user asked to restart into the installed update.
    pub restart: bool,
    pub settings_changed: bool,
}

impl Updater {
    pub fn state(&self) -> State {
        STATE.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn set(s: State) {
        if let Ok(mut g) = STATE.lock() {
            *g = s;
        }
    }

    /// Check GitHub in the background. `quiet` checks (at startup) skip source checkouts.
    pub fn check(&self, quiet: bool) {
        if quiet && (STARTUP_CHECKED.swap(true, Ordering::SeqCst) || install_dir().is_err()) {
            return;
        }
        if self.state().busy() || matches!(self.state(), State::Installed(_)) {
            return;
        }
        Self::set(State::Checking);
        let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || {
            let s = match latest_release() {
                Ok(r) if r.version > current_version() => State::Available(r),
                Ok(_) => State::UpToDate,
                Err(e) if quiet => {
                    log::info!("update check failed: {e}");
                    State::Idle
                }
                Err(e) => State::Failed(e),
            };
            if let State::Available(r) = &s {
                log::info!("Dumb Engine {} is available (Help ▸ Check for Updates)", r.tag);
            }
            Self::set(s);
        });
    }

    /// Download and install `release` in the background.
    pub fn install(&self, release: Release) {
        if self.state().busy() {
            return;
        }
        Self::set(State::Downloading { release: release.clone(), done: 0, total: 0 });
        let _ = std::thread::Builder::new().name("update-install".into()).spawn(move || {
            let r = download_and_install(&release, &|done, total| {
                Self::set(State::Downloading { release: release.clone(), done, total });
            }, &|| Self::set(State::Installing(release.clone())));
            Self::set(match r {
                Ok(()) => {
                    log::info!("Installed Dumb Engine {}; restart the editor to use it", release.tag);
                    State::Installed(release)
                }
                Err(e) => {
                    log::error!("update failed: {e}");
                    State::Failed(e)
                }
            });
        });
    }

    /// The new version, while one is available or installed (for the status bar).
    pub fn pending(&self) -> Option<(String, bool)> {
        match self.state() {
            State::Available(r) | State::Downloading { release: r, .. } | State::Installing(r) => Some((r.tag, false)),
            State::Installed(r) => Some((r.tag, true)),
            _ => None,
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, check_on_start: &mut bool) -> UpdaterOutput {
        let mut out = UpdaterOutput::default();
        if !self.open {
            return out;
        }
        let mut open = self.open;
        egui::Window::new("⬆ Updates").open(&mut open).resizable(false).default_width(460.0).collapsible(false).show(ctx, |ui| {
            ui.label(format!("Installed: Dumb Engine v{CURRENT}"));
            if let Err(why) = install_dir() {
                ui.weak(why);
            }
            ui.separator();
            match self.state() {
                State::Idle => {
                    if ui.button("Check for updates").clicked() {
                        self.check(false);
                    }
                }
                State::Checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Checking GitHub…");
                    });
                }
                State::UpToDate => {
                    ui.label("✔ You have the latest version.");
                    if ui.button("Check again").clicked() {
                        self.check(false);
                    }
                }
                State::Available(r) => {
                    ui.heading(format!("Dumb Engine {} is available", r.tag));
                    if !r.notes.trim().is_empty() {
                        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                            ui.label(r.notes.trim());
                        });
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let why_not = match (&r.asset, install_dir()) {
                            (None, _) => Some(format!("This release has no {} bundle.", platform().unwrap_or("download for this platform"))),
                            (_, Err(why)) => Some(why.to_string()),
                            _ => None,
                        };
                        let label = match &r.asset {
                            Some(a) => format!("⬇ Download and install ({:.0} MB)", a.size as f64 / 1e6),
                            None => "⬇ Download and install".into(),
                        };
                        let b = ui.add_enabled(why_not.is_none(), egui::Button::new(label));
                        if let Some(w) = &why_not {
                            b.on_disabled_hover_text(w);
                        } else if b.clicked() {
                            self.install(r.clone());
                        }
                        ui.hyperlink_to("View on GitHub", &r.page);
                    });
                }
                State::Downloading { release, done, total } => {
                    ui.label(format!("Downloading {}…", release.tag));
                    let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                    ui.add(egui::ProgressBar::new(frac).text(format!("{:.1} / {:.1} MB", done as f64 / 1e6, total as f64 / 1e6)));
                }
                State::Installing(r) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Installing {}…", r.tag));
                    });
                }
                State::Installed(r) => {
                    ui.label(format!("✔ {} is installed. Restart the editor to use it.", r.tag));
                    ui.weak("After the restart, press Build scripts once: game scripts must be rebuilt against the new engine.");
                    ui.add_space(4.0);
                    if ui.button("⟳ Restart now").clicked() {
                        out.restart = true;
                    }
                }
                State::Failed(e) => {
                    ui.colored_label(egui::Color32::from_rgb(230, 90, 80), format!("✖ {e}"));
                    if ui.button("Try again").clicked() {
                        self.check(false);
                    }
                }
            }
            ui.separator();
            out.settings_changed = ui.checkbox(check_on_start, "Check for updates when the editor starts").changed();
        });
        self.open = open;
        if out.restart {
            RESTART.store(true, Ordering::SeqCst);
        }
        out
    }
}

fn current_version() -> semver::Version {
    semver::Version::parse(CURRENT).expect("crate version is semver")
}

fn agent(timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(timeout)
        .user_agent(format!("dumb-engine-editor/{CURRENT}"))
        .build()
        .into()
}

fn latest_release() -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let mut resp = agent(Some(Duration::from_secs(20)))
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(404) => "No releases have been published yet.".to_string(),
            ureq::Error::StatusCode(403) => "GitHub is rate-limiting update checks. Try again later.".to_string(),
            e => format!("Could not reach GitHub: {e}"),
        })?;
    let text = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
    parse_release(&text, platform())
}

fn parse_release(json: &str, platform: Option<&str>) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("unexpected answer from GitHub: {e}"))?;
    let tag = v["tag_name"].as_str().ok_or("the release has no tag")?.to_string();
    let version = semver::Version::parse(tag.trim_start_matches('v')).map_err(|e| format!("tag {tag} is not a version: {e}"))?;
    let asset = platform.and_then(|p| {
        let stem = format!("DumbEngine-{tag}-{p}");
        v["assets"].as_array()?.iter().find_map(|a| {
            let name = a["name"].as_str()?;
            (name == format!("{stem}.zip") || name == format!("{stem}.tar.gz")).then(|| Asset {
                name: name.to_string(),
                url: a["browser_download_url"].as_str().unwrap_or_default().to_string(),
                size: a["size"].as_u64().unwrap_or(0),
            })
        })
    });
    Ok(Release {
        tag,
        version,
        notes: v["body"].as_str().unwrap_or_default().to_string(),
        page: v["html_url"].as_str().unwrap_or_default().to_string(),
        asset,
    })
}

fn download_and_install(release: &Release, progress: &dyn Fn(u64, u64), installing: &dyn Fn()) -> Result<(), String> {
    let dir = install_dir().map_err(str::to_string)?;
    let asset = release.asset.as_ref().ok_or("the release has no bundle for this platform")?;
    let work = dir.join(WORK);
    std::fs::create_dir_all(&work).map_err(|e| format!("cannot write to {}: {e}", work.display()))?;

    // Download.
    let archive = work.join(&asset.name);
    let mut resp = agent(None).get(&asset.url).call().map_err(|e| format!("download failed: {e}"))?;
    let total = resp.body().content_length().unwrap_or(asset.size);
    let mut reader = resp.body_mut().as_reader();
    let mut file = std::fs::File::create(&archive).map_err(|e| e.to_string())?;
    let (mut done, mut buf) = (0u64, vec![0u8; 256 * 1024]);
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("download failed: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        progress(done, total);
    }
    drop(file);
    if asset.size > 0 && done != asset.size {
        return Err(format!("download incomplete ({done} of {} bytes)", asset.size));
    }

    // Extract.
    installing();
    let staging = work.join("staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    extract(&archive, &staging)?;
    let root = bundle_root(&staging)?;

    let backup = work.join("backup");
    let _ = std::fs::remove_dir_all(&backup);
    install_bundle(&root, dir, &backup)?;
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_file(&archive);
    Ok(())
}

fn extract(archive: &Path, to: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let name = archive.to_string_lossy();
    if name.ends_with(".zip") {
        // `extract` refuses entries that would land outside `to`.
        zip::ZipArchive::new(file).and_then(|mut z| z.extract(to)).map_err(|e| format!("bad archive: {e}"))
    } else {
        // `unpack` refuses `..` and absolute paths.
        tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(to).map_err(|e| format!("bad archive: {e}"))
    }
}

/// The bundle folder inside the extracted archive (archives hold one top-level folder).
fn bundle_root(staging: &Path) -> Result<PathBuf, String> {
    let is_bundle = |p: &Path| p.join(format!("dumb-editor{EXE}")).is_file() && p.join("crates").join("dumb_script").is_dir();
    if is_bundle(staging) {
        return Ok(staging.to_path_buf());
    }
    std::fs::read_dir(staging)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir() && is_bundle(p))
        .ok_or_else(|| "the download is not a Dumb Engine bundle".into())
}

/// Move every top-level item of `bundle` into `dir`, moving what it replaces into `backup`
/// (which should be empty). All or nothing: on any error the moves made so far are undone.
fn install_bundle(bundle: &Path, dir: &Path, backup: &Path) -> Result<(), String> {
    std::fs::create_dir_all(backup).map_err(|e| e.to_string())?;
    let mut items: Vec<_> = std::fs::read_dir(bundle).map_err(|e| e.to_string())?.flatten().map(|e| e.file_name()).collect();
    items.sort();

    let mut backed_up = Vec::new();
    let mut placed = Vec::new();
    let result = (|| -> Result<(), String> {
        for name in &items {
            let target = dir.join(name);
            // The sample project may hold the user's own work.
            if name == "project" && target.exists() {
                continue;
            }
            if target.exists() {
                std::fs::rename(&target, backup.join(name)).map_err(|e| format!("cannot replace {}: {e} (is it open in another program?)", target.display()))?;
                backed_up.push(name.clone());
            }
            std::fs::rename(bundle.join(name), &target).map_err(|e| format!("cannot install {}: {e}", target.display()))?;
            placed.push(name.clone());
        }
        Ok(())
    })();

    if result.is_err() {
        for name in placed.iter().rev() {
            let _ = std::fs::rename(dir.join(name), bundle.join(name));
        }
        for name in backed_up.iter().rev() {
            let _ = std::fs::rename(backup.join(name), dir.join(name));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_release() {
        let json = r#"{
            "tag_name": "v0.2.0", "body": "notes", "html_url": "https://example.test/r",
            "assets": [
                {"name": "DumbEngine-v0.2.0-linux-x64.tar.gz", "browser_download_url": "https://example.test/l", "size": 5},
                {"name": "DumbEngine-v0.2.0-windows-x64.zip", "browser_download_url": "https://example.test/w", "size": 7}
            ]
        }"#;
        let r = parse_release(json, Some("windows-x64")).unwrap();
        assert_eq!(r.version, semver::Version::new(0, 2, 0));
        let a = r.asset.unwrap();
        assert_eq!((a.name.as_str(), a.size), ("DumbEngine-v0.2.0-windows-x64.zip", 7));
        assert!(parse_release(json, Some("macos-arm64")).unwrap().asset.is_none());
        assert!(parse_release(r#"{"tag_name": "nightly"}"#, None).is_err());
    }

    /// Network: the real latest bundle extracts and installs. `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_release_installs() {
        let release = latest_release().unwrap();
        let asset = release.asset.expect("bundle for this platform");
        let work = temp("real");
        let archive = work.join(&asset.name);
        let mut resp = agent(None).get(&asset.url).call().unwrap();
        std::io::copy(&mut resp.body_mut().as_reader(), &mut std::fs::File::create(&archive).unwrap()).unwrap();
        assert_eq!(std::fs::metadata(&archive).unwrap().len(), asset.size);
        let staging = work.join("staging");
        extract(&archive, &staging).unwrap();
        let root = bundle_root(&staging).unwrap();
        let dir = work.join("install");
        write(&dir.join("project/mine.txt"), "user");
        install_bundle(&root, &dir, &work.join("backup")).unwrap();
        assert!(dir.join(format!("dumb-editor{EXE}")).is_file());
        assert!(dir.join("crates/dumb_script").is_dir());
        assert!(dir.join("rust-toolchain.toml").is_file());
        assert!(!dir.join("project/project.ron").exists(), "the sample project must not overwrite the user's");
        let _ = std::fs::remove_dir_all(&work);
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dumb_update_{name}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn installs_in_place_and_keeps_user_files() {
        let (dir, bundle) = (temp("dir"), temp("bundle"));
        write(&dir.join("dumb-editor.exe"), "old");
        write(&dir.join("crates/dumb_script/lib.rs"), "old");
        write(&dir.join("crates/gone.rs"), "old");
        write(&dir.join("project/mine.txt"), "user");
        write(&dir.join("dumb-editor.exe.WebView2/state"), "user");
        write(&bundle.join("dumb-editor.exe"), "new");
        write(&bundle.join("crates/dumb_script/lib.rs"), "new");
        write(&bundle.join("project/sample.txt"), "sample");
        write(&bundle.join("START_HERE.txt"), "new");

        install_bundle(&bundle, &dir, &dir.join(".update/backup")).unwrap();
        let read = |p: &str| std::fs::read_to_string(dir.join(p)).unwrap();
        assert_eq!(read("dumb-editor.exe"), "new");
        assert_eq!(read("crates/dumb_script/lib.rs"), "new");
        assert!(!dir.join("crates/gone.rs").exists(), "crates/ is replaced as a whole");
        assert_eq!(read("project/mine.txt"), "user");
        assert!(!dir.join("project/sample.txt").exists());
        assert_eq!(read("dumb-editor.exe.WebView2/state"), "user");
        assert_eq!(read("START_HERE.txt"), "new");
        assert_eq!(read(".update/backup/dumb-editor.exe"), "old");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&bundle);
    }

    #[test]
    fn failed_install_rolls_back() {
        let (dir, bundle) = (temp("dir"), temp("bundle"));
        write(&dir.join("a.txt"), "old");
        write(&dir.join("b.txt"), "old");
        write(&bundle.join("a.txt"), "new");
        write(&bundle.join("b.txt"), "new");
        // `a.txt` installs first; backing up `b.txt` then fails because its backup slot is a
        // non-empty directory, so `a.txt` must be put back.
        let backup = dir.join(".update/backup");
        write(&backup.join("b.txt/blocker"), "x");

        assert!(install_bundle(&bundle, &dir, &backup).is_err());
        let read = |p: &Path| std::fs::read_to_string(p).unwrap();
        assert_eq!(read(&dir.join("a.txt")), "old");
        assert_eq!(read(&dir.join("b.txt")), "old");
        assert_eq!(read(&bundle.join("a.txt")), "new");
        assert_eq!(read(&bundle.join("b.txt")), "new");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&bundle);
    }
}
