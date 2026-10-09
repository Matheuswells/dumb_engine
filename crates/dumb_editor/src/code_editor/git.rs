//! Git integration for the code editor: status, per-line diff markers, commit, pull/push, log.
//! Everything shells out to the `git` command line.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir).env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().map_err(|e| format!("git not found: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { String::from_utf8_lossy(&out.stdout).trim().to_string() } else { err })
    }
}

/// Root of the repository containing `dir`, if any.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    run(dir, &["rev-parse", "--show-toplevel"]).ok().map(|s| PathBuf::from(s.trim()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStatus {
    /// Two-letter porcelain code (`M `, ` M`, `??`, `A `...).
    pub code: String,
    pub path: PathBuf,
}

impl FileStatus {
    pub fn letter(&self) -> &'static str {
        match self.code.trim() {
            "??" => "U",
            c if c.contains('A') => "A",
            c if c.contains('D') => "D",
            c if c.contains('R') => "R",
            c if c.contains('M') => "M",
            _ => "•",
        }
    }
}

pub fn status(root: &Path) -> Result<Vec<FileStatus>, String> {
    let s = run(root, &["status", "--porcelain=v1", "-uall"])?;
    Ok(parse_status(&s, root))
}

pub fn parse_status(s: &str, root: &Path) -> Vec<FileStatus> {
    s.lines()
        .filter(|l| l.len() > 3)
        .map(|l| {
            let path = l[3..].rsplit(" -> ").next().unwrap_or(&l[3..]).trim_matches('"');
            FileStatus { code: l[..2].to_string(), path: root.join(path) }
        })
        .collect()
}

pub fn branch(root: &Path) -> String {
    run(root, &["rev-parse", "--abbrev-ref", "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_else(|_| "(no commits)".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineChange {
    Added,
    Modified,
    /// Lines were removed just above this line.
    Deleted,
}

/// Per-line changes of the working file vs HEAD (0-based line numbers).
pub fn line_changes(root: &Path, file: &Path) -> Vec<(usize, LineChange)> {
    let Ok(rel) = file.strip_prefix(root) else { return Vec::new() };
    let rel = rel.to_string_lossy().replace('\\', "/");
    if run(root, &["ls-files", "--error-unmatch", &rel]).is_err() {
        // Untracked: everything is new.
        let n = std::fs::read_to_string(file).map(|s| s.lines().count()).unwrap_or(0);
        return (0..n).map(|l| (l, LineChange::Added)).collect();
    }
    match run(root, &["diff", "--no-color", "--unified=0", "HEAD", "--", &rel]) {
        Ok(d) => parse_diff(&d),
        Err(_) => Vec::new(),
    }
}

/// Parse `@@ -a,b +c,d @@` hunks of a zero-context diff.
pub fn parse_diff(diff: &str) -> Vec<(usize, LineChange)> {
    let mut out = Vec::new();
    for l in diff.lines().filter(|l| l.starts_with("@@")) {
        let mut parts = l.split_whitespace().skip(1);
        let (Some(old), Some(new)) = (parts.next(), parts.next()) else { continue };
        let range = |s: &str| -> (usize, usize) {
            let s = &s[1..];
            let (a, b) = s.split_once(',').unwrap_or((s, "1"));
            (a.parse().unwrap_or(0), b.parse().unwrap_or(1))
        };
        let (_, old_n) = range(old);
        let (new_start, new_n) = range(new);
        if new_n == 0 {
            out.push((new_start, LineChange::Deleted)); // removed after line `new_start` (1-based)
        } else {
            let kind = if old_n == 0 { LineChange::Added } else { LineChange::Modified };
            for i in 0..new_n {
                out.push((new_start + i - 1, kind));
            }
        }
    }
    out
}

pub fn log(root: &Path, n: usize) -> Vec<String> {
    run(root, &["log", "--no-color", &format!("-n{n}"), "--pretty=format:%h  %s  (%ar, %an)"])
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

pub fn diff_text(root: &Path, file: Option<&Path>) -> String {
    let mut args = vec!["diff", "--no-color", "HEAD"];
    let rel;
    if let Some(f) = file {
        rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().replace('\\', "/");
        args.push("--");
        args.push(&rel);
    }
    run(root, &args).unwrap_or_else(|e| e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hunks() {
        let d = "diff --git a/x b/x\n@@ -3,0 +4,2 @@\n+a\n+b\n@@ -10,2 +12,2 @@\n-x\n-y\n+X\n+Y\n@@ -20,3 +21,0 @@\n-gone\n";
        let c = parse_diff(d);
        assert_eq!(c, vec![(3, LineChange::Added), (4, LineChange::Added), (11, LineChange::Modified), (12, LineChange::Modified), (21, LineChange::Deleted)]);
    }

    #[test]
    fn parses_status() {
        let s = parse_status(" M src/a.rs\n?? src/new.rs\nR  old.rs -> new.rs\n", Path::new("/r"));
        assert_eq!(s[0].letter(), "M");
        assert_eq!(s[1].letter(), "U");
        assert_eq!(s[2].path, Path::new("/r").join("new.rs"));
    }
}
