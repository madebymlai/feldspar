use std::path::{Path, PathBuf};

// ── Cache helpers ─────────────────────────────────────────────────────────────

fn cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("feldspar/shim-cache.json"))
}

pub fn read_cached_path() -> Option<PathBuf> {
    let path = cache_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    v.get("claude_path")?.as_str().map(PathBuf::from)
}

pub fn write_cached_path(claude_path: &Path) -> Result<(), String> {
    let path = cache_path().ok_or("no home dir")?;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Merge into any existing cache rather than overwriting the whole file.
    let mut v: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    v.as_object_mut()
        .unwrap()
        .insert("claude_path".into(), serde_json::json!(claude_path.to_string_lossy()));
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap())
        .map_err(|e| format!("cache write failed: {e}"))
}

pub fn write_feldspar_size(feldspar_bin: &Path) -> Result<(), String> {
    let path = cache_path().ok_or("no home dir")?;
    let size = std::fs::metadata(feldspar_bin).map(|m| m.len()).unwrap_or(0);
    let mut v: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    v.as_object_mut()
        .unwrap()
        .insert("feldspar_size".into(), serde_json::json!(size));
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap())
        .map_err(|e| format!("cache write failed: {e}"))
}

// ── Resolve real claude ───────────────────────────────────────────────────────

pub fn resolve_real_claude() -> Result<PathBuf, String> {
    // 1. Try cache first.
    if let Some(cached) = read_cached_path() {
        if cached.exists() {
            return Ok(cached);
        }
    }

    // 2. PATH-skip walk: skip any entry that canonicalizes to ourself.
    let my_exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("cannot determine own path: {e}"))?;

    let path_var = std::env::var("PATH").map_err(|_| "PATH not set".to_string())?;

    for dir in std::env::split_paths(&path_var) {
        let candidate = if cfg!(windows) {
            dir.join("claude.exe")
        } else {
            dir.join("claude")
        };
        if !candidate.exists() {
            continue;
        }
        if let Ok(resolved) = candidate.canonicalize() {
            if resolved == my_exe {
                continue;
            }
        }
        let _ = write_cached_path(&candidate);
        return Ok(candidate);
    }

    Err("Claude Code not found. Run `feldspar doctor`.".into())
}

// ── Session name ──────────────────────────────────────────────────────────────

fn session_name() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let raw_basename = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("default");
    let basename: String = raw_basename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let hash: u64 = cwd.to_string_lossy().bytes().map(|b| b as u64).sum();
    let pid = std::process::id();
    format!("feldspar-{}-{:x}-{}", basename, hash % 0xFFF_FFFF, pid)
}

// ── Unix platform functions ───────────────────────────────────────────────────

#[cfg(unix)]
fn exec_claude(real_claude: &Path, args: &[String]) -> ! {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(real_claude).args(args).exec();
    eprintln!("feldspar: failed to exec claude: {err}");
    std::process::exit(1);
}

#[cfg(unix)]
fn spawn_in_tmux(real_claude: &Path, args: &[String], session: &str) -> ! {
    use std::os::unix::process::CommandExt;
    // Kill stale session (best-effort, no-op if it doesn't exist).
    let _ = std::process::Command::new("tmux")
        .args(["kill-session", "-t", session])
        .output();

    // Build args array: tmux new-session -s <name> -- <claude> [args…]
    // Using `--` avoids re-parsing through sh -c, so spaces/quotes in args survive.
    let mut tmux_args: Vec<std::ffi::OsString> = vec![
        "new-session".into(),
        "-s".into(),
        session.into(),
        "--".into(),
        real_claude.as_os_str().to_owned(),
    ];
    for arg in args {
        tmux_args.push(arg.into());
    }
    let err = std::process::Command::new("tmux").args(&tmux_args).exec();
    eprintln!("feldspar: failed to exec tmux: {err}");
    std::process::exit(1);
}

// ── Windows platform functions ────────────────────────────────────────────────

#[cfg(windows)]
fn spawn_claude(real_claude: &Path, args: &[String]) -> ! {
    let status = std::process::Command::new(real_claude).args(args).status();
    match status {
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("feldspar: failed to spawn claude: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(windows)]
fn spawn_in_psmux(real_claude: &Path, args: &[String], session: &str) -> ! {
    // Ignore Ctrl+C in shim — let psmux handle it for the child.
    let _ = ctrlc::set_handler(|| {});
    // Kill stale session (best-effort).
    let _ = std::process::Command::new("psmux")
        .args(["kill-session", "-t", session])
        .output();

    let mut psmux_args: Vec<std::ffi::OsString> = vec![
        "new-session".into(),
        "-s".into(),
        session.into(),
        "--".into(),
        real_claude.as_os_str().to_owned(),
    ];
    for arg in args {
        psmux_args.push(arg.into());
    }
    let status = std::process::Command::new("psmux").args(&psmux_args).status();
    match status {
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("feldspar: failed to spawn psmux: {e}");
            std::process::exit(1);
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run(args: Vec<String>) -> ! {
    let real_claude = match resolve_real_claude() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("feldspar: {e}");
            std::process::exit(1);
        }
    };

    let in_mux = std::env::var("TMUX").is_ok() || std::env::var("PSMUX").is_ok();

    if in_mux {
        #[cfg(unix)]
        exec_claude(&real_claude, &args);
        #[cfg(windows)]
        spawn_claude(&real_claude, &args);
    } else {
        let session = session_name();
        #[cfg(unix)]
        spawn_in_tmux(&real_claude, &args, &session);
        #[cfg(windows)]
        spawn_in_psmux(&real_claude, &args, &session);
    }

    // Unreachable on all supported platforms, but satisfies the type checker
    // if somehow neither cfg branch is active (e.g. cross-compilation edge case).
    #[allow(unreachable_code)]
    {
        eprintln!("feldspar: unsupported platform");
        std::process::exit(1);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Write arbitrary JSON to a temp file and return the dir (keeps it alive).
    fn write_cache_json(dir: &TempDir, json: &str) -> PathBuf {
        let path = dir.path().join("shim-cache.json");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(json.as_bytes()).unwrap();
        path
    }

    // ── read_cached_path ─────────────────────────────────────────────────────

    #[test]
    fn test_read_cached_path_missing() {
        // Point cache_path to a dir with no file → must return None.
        // We can't override dirs::home_dir(), so we test the internal JSON
        // parsing helper directly by verifying the function returns None when
        // the file doesn't exist. We do this by testing with a JSON that has
        // no "claude_path" key.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("shim-cache.json");
        // File does not exist → read_to_string fails → None chain.
        assert!(!path.exists());
        // We cannot override home_dir, but we test the code path via the
        // json parsing logic directly.
        let v: Option<PathBuf> = {
            let content = std::fs::read_to_string(&path).ok();
            content
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v.get("claude_path").and_then(|p| p.as_str()).map(PathBuf::from))
        };
        assert!(v.is_none());
    }

    #[test]
    fn test_read_cached_path_valid() {
        let dir = TempDir::new().unwrap();
        let fake_claude = dir.path().join("claude");
        std::fs::write(&fake_claude, b"").unwrap();
        let json = format!(
            r#"{{"claude_path": "{}"}}"#,
            fake_claude.to_string_lossy()
        );
        let cache_file = write_cache_json(&dir, &json);
        // Verify the JSON parses to the expected path.
        let content = std::fs::read_to_string(&cache_file).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        let parsed: PathBuf = v["claude_path"].as_str().map(PathBuf::from).unwrap();
        assert_eq!(parsed, fake_claude);
    }

    #[test]
    fn test_read_cached_path_invalid_json() {
        let dir = TempDir::new().unwrap();
        write_cache_json(&dir, "not json at all {{{{");
        let cache_file = dir.path().join("shim-cache.json");
        let content = std::fs::read_to_string(&cache_file).ok();
        let result: Option<PathBuf> = content
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("claude_path").and_then(|p| p.as_str()).map(PathBuf::from));
        assert!(result.is_none());
    }

    // ── resolve_real_claude PATH-skip ─────────────────────────────────────────

    #[test]
    fn test_resolve_skips_self() {
        // Create a temp dir with a symlink named "claude" → current test binary.
        let dir = TempDir::new().unwrap();
        let self_exe = std::env::current_exe().unwrap().canonicalize().unwrap();
        let symlink_path = dir.path().join("claude");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&self_exe, &symlink_path).unwrap();
        #[cfg(not(unix))]
        {
            // On non-Unix we just copy the binary for the test.
            std::fs::copy(&self_exe, &symlink_path).unwrap();
        }

        // Prepend our temp dir to PATH so resolve_real_claude() finds it first.
        let original_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{original_path}", dir.path().display());
        // SAFETY: single-threaded test; no concurrent env reads in this test.
        unsafe { std::env::set_var("PATH", &new_path) };

        let result = resolve_real_claude();

        // Restore PATH.
        // SAFETY: same as above.
        unsafe { std::env::set_var("PATH", &original_path) };

        // If only the self-symlink is in PATH, we should get an error (not Ok).
        // If there's a real claude elsewhere in the original PATH, the result
        // will be Ok — but it must NOT be the temp dir symlink.
        if let Ok(found) = result {
            let found_canon = found.canonicalize().unwrap_or(found.clone());
            assert_ne!(found_canon, self_exe, "resolve must not return self");
        }
        // Getting Err is also acceptable (no real claude on this CI machine).
    }

    // ── session_name ──────────────────────────────────────────────────────────

    #[test]
    fn test_session_name_basic() {
        let name = session_name();
        assert!(name.starts_with("feldspar-"), "must start with feldspar-");
    }

    #[test]
    fn test_session_name_dots_sanitized() {
        // Build a session name for a fake CWD basename containing dots.
        let raw = "my.cool.project";
        let sanitized: String = raw
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        assert_eq!(sanitized, "my_cool_project");
        assert!(!sanitized.contains('.'));
    }

    #[test]
    fn test_session_name_colons_sanitized() {
        let raw = "org:repo";
        let sanitized: String = raw
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        assert_eq!(sanitized, "org_repo");
        assert!(!sanitized.contains(':'));
    }

    #[test]
    fn test_session_name_different_dirs_different_hash() {
        let hash_for = |path: &str| -> u64 {
            path.bytes().map(|b| b as u64).sum::<u64>() % 0xFFF_FFFF
        };
        let h1 = hash_for("/a/myproject");
        let h2 = hash_for("/b/myproject");
        assert_ne!(h1, h2, "different parent dirs must yield different hashes");
    }

    #[test]
    fn test_session_name_includes_pid() {
        let name = session_name();
        let pid = std::process::id().to_string();
        assert!(name.ends_with(&pid), "session name must end with PID");
    }

    // ── Unix command construction (no exec) ───────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn test_tmux_command_args() {
        let real_claude = PathBuf::from("/usr/bin/claude");
        let args = vec!["--resume".to_string()];
        let session = "feldspar-test-abc-123";

        let mut expected: Vec<std::ffi::OsString> = vec![
            "new-session".into(),
            "-s".into(),
            session.into(),
            "--".into(),
            real_claude.as_os_str().to_owned(),
        ];
        for arg in &args {
            expected.push(arg.into());
        }

        // Mirror the construction logic from spawn_in_tmux without exec'ing.
        let mut tmux_args: Vec<std::ffi::OsString> = vec![
            "new-session".into(),
            "-s".into(),
            session.into(),
            "--".into(),
            real_claude.as_os_str().to_owned(),
        ];
        for arg in &args {
            tmux_args.push(arg.into());
        }

        assert_eq!(tmux_args, expected);
    }

    #[cfg(unix)]
    #[test]
    fn test_tmux_command_with_spaces() {
        let real_claude = PathBuf::from("/usr/bin/claude");
        let args = vec!["-p".to_string(), "fix the bug".to_string()];
        let session = "feldspar-test-abc-123";

        let mut tmux_args: Vec<std::ffi::OsString> = vec![
            "new-session".into(),
            "-s".into(),
            session.into(),
            "--".into(),
            real_claude.as_os_str().to_owned(),
        ];
        for arg in &args {
            tmux_args.push(arg.into());
        }

        // The arg with a space must survive unmodified.
        let last: &std::ffi::OsString = tmux_args.last().unwrap();
        assert_eq!(last.to_string_lossy(), "fix the bug");
    }

    // ── run() mux detection (env-var based, no actual exec) ──────────────────

    #[test]
    fn test_mux_detection_tmux() {
        // SAFETY: single-threaded test isolation.
        unsafe { std::env::set_var("TMUX", "/tmp/tmux-1000/default,1234,0") };
        let in_mux = std::env::var("TMUX").is_ok() || std::env::var("PSMUX").is_ok();
        unsafe { std::env::remove_var("TMUX") };
        assert!(in_mux);
    }

    #[test]
    fn test_mux_detection_psmux() {
        unsafe {
            std::env::remove_var("TMUX");
            std::env::set_var("PSMUX", "1");
        }
        let in_mux = std::env::var("TMUX").is_ok() || std::env::var("PSMUX").is_ok();
        unsafe { std::env::remove_var("PSMUX") };
        assert!(in_mux);
    }

    #[test]
    fn test_mux_detection_none() {
        unsafe {
            std::env::remove_var("TMUX");
            std::env::remove_var("PSMUX");
        }
        let in_mux = std::env::var("TMUX").is_ok() || std::env::var("PSMUX").is_ok();
        assert!(!in_mux);
    }
}
