//! Blender as a first-class importer.
//!
//! `.blend` files (and formats Rust has no good loader for — FBX, OBJ, DAE, STL, PLY, USD) are
//! converted by running Blender headless, which exports a GLB into the project's `Library/`
//! cache. The GLB is then imported by the glTF importer. Because the watcher tracks the
//! `.blend` itself, simply hitting Ctrl+S in Blender re-imports the asset in the editor.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Find a Blender executable: `DUMB_BLENDER` env var, PATH, then common install locations.
pub fn find_blender() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("DUMB_BLENDER") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let exe = if cfg!(windows) { "blender.exe" } else { "blender" };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join(exe);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for drive in ["C", "D", "E"] {
            candidates.push(format!(r"{drive}:\steam\steamapps\common\Blender\blender.exe").into());
            candidates.push(format!(r"{drive}:\SteamLibrary\steamapps\common\Blender\blender.exe").into());
            candidates.push(format!(r"{drive}:\Program Files (x86)\Steam\steamapps\common\Blender\blender.exe").into());
            let foundation = PathBuf::from(format!(r"{drive}:\Program Files\Blender Foundation"));
            if let Ok(rd) = std::fs::read_dir(&foundation) {
                let mut versions: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("blender.exe")).collect();
                versions.sort();
                versions.reverse();
                candidates.extend(versions);
            }
        }
    } else if cfg!(target_os = "macos") {
        candidates.push("/Applications/Blender.app/Contents/MacOS/Blender".into());
    } else {
        candidates.push("/usr/bin/blender".into());
        candidates.push("/snap/bin/blender".into());
    }
    candidates.into_iter().find(|p| p.exists())
}

/// Python run inside Blender. Arguments after `--`: input path, output .glb path.
const CONVERT_SCRIPT: &str = r#"
import bpy, sys, os
argv = sys.argv[sys.argv.index("--") + 1:]
src, out = argv[0], argv[1]
ext = os.path.splitext(src)[1].lower()
if ext != ".blend":
    bpy.ops.wm.read_factory_settings(use_empty=True)
    if ext == ".fbx":
        bpy.ops.import_scene.fbx(filepath=src)
    elif ext == ".obj":
        bpy.ops.wm.obj_import(filepath=src)
    elif ext == ".stl":
        bpy.ops.wm.stl_import(filepath=src)
    elif ext == ".ply":
        bpy.ops.wm.ply_import(filepath=src)
    elif ext in (".usd", ".usda", ".usdc", ".usdz"):
        bpy.ops.wm.usd_import(filepath=src)
    elif ext == ".dae":
        bpy.ops.wm.collada_import(filepath=src)
    else:
        raise SystemExit("unsupported format " + ext)
os.makedirs(os.path.dirname(out), exist_ok=True)
bpy.ops.export_scene.gltf(
    filepath=out,
    export_format="GLB",
    export_apply=True,
    export_animations=True,
    export_skins=True,
    export_yup=True,
    export_extras=True,
)
print("DUMB_EXPORT_OK", out)
"#;

/// Convert `src` to a GLB at `out` using Blender. Blocks until Blender exits.
pub fn convert_to_glb(blender: &Path, src: &Path, out: &Path) -> Result<(), String> {
    let script = std::env::temp_dir().join("dumb_engine_blender_export.py");
    std::fs::write(&script, CONVERT_SCRIPT).map_err(|e| e.to_string())?;

    let mut cmd = Command::new(blender);
    cmd.arg("--background").arg("--factory-startup");
    if src.extension().is_some_and(|e| e.eq_ignore_ascii_case("blend")) {
        cmd.arg(src);
    }
    cmd.arg("--python").arg(&script).arg("--").arg(src).arg(out);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = cmd.output().map_err(|e| format!("failed to run Blender ({}): {e}", blender.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("DUMB_EXPORT_OK") || !out.exists() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stdout.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        return Err(format!("Blender export failed for {}\n{tail}\n{stderr}", src.display()));
    }
    Ok(())
}

/// Extensions routed through Blender.
pub const BLENDER_FORMATS: &[&str] = &["blend", "fbx", "obj", "stl", "ply", "usd", "usda", "usdc", "usdz", "dae"];
