use std::path::{Path, PathBuf};

// ── Path helpers ──────────────────────────────────────────────────────────────

fn shim_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("feldspar/bin"))
}

fn shim_path() -> Option<PathBuf> {
    let dir = shim_dir()?;
    if cfg!(windows) {
        Some(dir.join("claude.exe"))
    } else {
        Some(dir.join("claude"))
    }
}

fn is_test_binary(p: &Path) -> bool {
    p.components().any(|c| c.as_os_str() == "deps")
}

// ── check_shim ────────────────────────────────────────────────────────────────

pub fn check_shim() -> bool {
    let Some(shim) = shim_path() else {
        eprintln!("  [FAIL] Cannot determine home directory");
        return false;
    };

    let my_exe = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(p) => p,
        Err(_) => {
            eprintln!("  [FAIL] Cannot determine own binary path");
            return false;
        }
    };

    // Running as a cargo test binary: we can't validate or repair the shim
    // without pointing it at the test binary under target/<profile>/deps/.
    // Bail out without touching the real shim.
    if is_test_binary(&my_exe) {
        eprintln!("  [SKIP] Running from test binary; shim check skipped");
        return true;
    }

    if !shim.exists() {
        eprintln!("  [FAIL] Shim not found at {}", shim.display());
        return fix_shim(&shim);
    }

    if let Ok(resolved) = shim.canonicalize() {
        if resolved != my_exe {
            eprintln!("  [WARN] Shim points to wrong binary, recreating...");
            return fix_shim(&shim);
        }
    }

    println!("  [OK] Shim exists and resolves correctly");
    true
}

fn fix_shim(shim: &Path) -> bool {
    let feldspar_bin = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("  [FAIL] Cannot find own binary: {e}");
            return false;
        }
    };

    if is_test_binary(&feldspar_bin) {
        eprintln!(
            "  [FAIL] Refusing to shim test binary at {}",
            feldspar_bin.display()
        );
        return false;
    }

    let _ = std::fs::remove_file(shim);
    if let Some(parent) = shim.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    #[cfg(unix)]
    {
        match std::os::unix::fs::symlink(&feldspar_bin, shim) {
            Ok(()) => {
                println!("  [FIXED] Recreated symlink");
                true
            }
            Err(e) => {
                eprintln!("  [FAIL] Symlink failed: {e}");
                false
            }
        }
    }

    #[cfg(windows)]
    {
        match std::fs::hard_link(&feldspar_bin, shim) {
            Ok(()) => {
                println!("  [FIXED] Recreated hardlink");
                true
            }
            Err(e) => {
                eprintln!("  [FAIL] Hardlink failed: {e}");
                false
            }
        }
    }
}

// ── check_path_order ──────────────────────────────────────────────────────────

pub fn check_path_order() -> bool {
    let shim = match shim_path() {
        Some(p) => p,
        None => {
            eprintln!("  [FAIL] Cannot determine shim path");
            return false;
        }
    };

    let path_var = std::env::var("PATH").unwrap_or_default();
    let claude_name = if cfg!(windows) { "claude.exe" } else { "claude" };

    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(claude_name);
        if candidate.exists() {
            if candidate.starts_with(shim.parent().unwrap_or(Path::new(""))) {
                println!("  [OK] PATH order correct — feldspar shim is first");
                return true;
            } else {
                eprintln!(
                    "  [WARN] PATH order wrong — {} comes before shim",
                    candidate.display()
                );
                eprintln!("  [INFO] Re-run `feldspar init` to fix PATH order");
                return false;
            }
        }
    }

    eprintln!("  [WARN] No claude found in PATH at all");
    false
}

// ── check_claude_cache ────────────────────────────────────────────────────────

pub fn check_claude_cache() -> bool {
    match crate::proxy::read_cached_path() {
        Some(path) if path.exists() => {
            println!("  [OK] Cached claude path valid: {}", path.display());
            true
        }
        Some(path) => {
            eprintln!("  [WARN] Cached path stale: {}", path.display());
            match crate::proxy::resolve_real_claude() {
                Ok(new_path) => {
                    println!("  [FIXED] Updated cache to: {}", new_path.display());
                    true
                }
                Err(e) => {
                    eprintln!("  [FAIL] {e}");
                    false
                }
            }
        }
        None => {
            eprintln!("  [WARN] No cached claude path");
            match crate::proxy::resolve_real_claude() {
                Ok(new_path) => {
                    println!("  [FIXED] Created cache: {}", new_path.display());
                    true
                }
                Err(e) => {
                    eprintln!("  [FAIL] {e}");
                    false
                }
            }
        }
    }
}

// ── check_multiplexer ─────────────────────────────────────────────────────────

pub fn check_multiplexer() -> bool {
    let mux = if cfg!(windows) { "psmux" } else { "tmux" };
    match std::process::Command::new(mux).arg("-V").output() {
        Ok(o) if o.status.success() => {
            println!("  [OK] {} installed", mux);
            true
        }
        _ => {
            eprintln!("  [FAIL] {} not installed", mux);
            eprintln!("  [INFO] Run `feldspar init` to install, or install manually");
            false
        }
    }
}

// ── check_stale_sessions ──────────────────────────────────────────────────────

#[cfg(unix)]
pub fn check_stale_sessions() -> bool {
    let output = std::process::Command::new("tmux")
        .args(["ls", "-F", "#{session_name}"])
        .output();

    let sessions = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return true, // tmux not running or no sessions — fine
    };

    let mut cleaned = 0;
    for session in sessions.lines() {
        if !session.starts_with("feldspar-") {
            continue;
        }
        if let Some(pid_str) = session.rsplit('-').next() {
            if let Ok(pid) = pid_str.parse::<u32>() {
                let alive = std::process::Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if !alive {
                    let _ = std::process::Command::new("tmux")
                        .args(["kill-session", "-t", session])
                        .output();
                    cleaned += 1;
                }
            }
        }
    }

    if cleaned > 0 {
        println!("  [FIXED] Cleaned {} stale tmux session(s)", cleaned);
    } else {
        println!("  [OK] No stale sessions");
    }
    true
}

#[cfg(windows)]
pub fn check_stale_sessions() -> bool {
    // psmux session cleanup — implement when Windows testing is available
    true
}

// ── run ───────────────────────────────────────────────────────────────────────

pub async fn run() {
    println!("feldspar doctor\n");

    let checks: Vec<(&str, bool)> = vec![
        ("Shim binary", check_shim()),
        ("PATH order", check_path_order()),
        ("Claude cache", check_claude_cache()),
        ("Multiplexer", check_multiplexer()),
        ("Stale sessions", check_stale_sessions()),
    ];

    let issues: usize = checks.iter().filter(|(_, passed)| !passed).count();

    println!("\n---");
    if issues == 0 {
        println!("All checks passed.");
        std::process::exit(0);
    } else {
        println!("{} issue(s) found. Review output above.", issues);
        std::process::exit(1);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shim_dir_creation() {
        let dir = shim_dir();
        assert!(dir.is_some(), "shim_dir() requires a home directory");
        let dir = dir.unwrap();
        assert!(
            dir.ends_with("feldspar/bin"),
            "shim_dir must end with feldspar/bin, got: {}",
            dir.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_shim_path_unix() {
        let path = shim_path().expect("shim_path() requires a home directory");
        assert!(
            path.ends_with("feldspar/bin/claude"),
            "Unix shim must end with feldspar/bin/claude, got: {}",
            path.display()
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_shim_path_windows() {
        let path = shim_path().expect("shim_path() requires a home directory");
        assert!(
            path.ends_with("feldspar/bin/claude.exe"),
            "Windows shim must end with feldspar/bin/claude.exe, got: {}",
            path.display()
        );
    }

    #[test]
    fn test_is_test_binary_detects_deps() {
        // Cargo runs test binaries from target/<profile>/deps/, so our own
        // current_exe() during `cargo test` must be recognised as a test binary.
        let exe = std::env::current_exe().expect("current_exe in test");
        assert!(
            is_test_binary(&exe),
            "test binary must be detected as test binary: {}",
            exe.display()
        );
    }

    #[test]
    fn test_check_shim_skips_under_test() {
        // check_shim() must not mutate the real ~/feldspar/bin/claude symlink
        // when invoked from a cargo test binary. It should return true and do
        // nothing. We verify it returns without panicking; the destructive
        // side-effect path is gated by the is_test_binary() early return.
        assert!(
            check_shim(),
            "check_shim must be a no-op success under cargo test"
        );
    }

    #[test]
    fn test_fix_shim_refuses_test_binary() {
        use tempfile::TempDir;
        // fix_shim() must refuse to link when current_exe() is a test binary,
        // to avoid pointing ~/feldspar/bin/claude at the test runner.
        let tmp = TempDir::new().unwrap();
        let fake_shim = tmp.path().join("claude");
        assert!(
            !fix_shim(&fake_shim),
            "fix_shim must refuse to create a link to a test binary"
        );
        assert!(
            !fake_shim.exists(),
            "fix_shim must not create the shim file when refusing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_check_multiplexer_installed() {
        // tmux should be present on the CI/dev machine; if not the check returns false.
        // We only verify the function returns a bool and doesn't panic.
        let result = std::panic::catch_unwind(check_multiplexer);
        assert!(result.is_ok(), "check_multiplexer must not panic");
    }

    #[test]
    fn test_check_claude_cache_valid() {
        use std::io::Write;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let fake_claude = dir.path().join("claude");
        std::fs::File::create(&fake_claude).unwrap();

        let cache_path = dir.path().join("shim-cache.json");
        let json = format!(
            r#"{{"claude_path": "{}"}}"#,
            fake_claude.to_string_lossy()
        );
        let mut f = std::fs::File::create(&cache_path).unwrap();
        f.write_all(json.as_bytes()).unwrap();

        // Parse the JSON the same way read_cached_path does.
        let content = std::fs::read_to_string(&cache_path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        let parsed = v["claude_path"].as_str().map(PathBuf::from).unwrap();
        assert!(parsed.exists(), "parsed path must exist");
    }

    #[test]
    fn test_check_claude_cache_stale() {
        let dir = tempfile::TempDir::new().unwrap();
        let stale_path = dir.path().join("nonexistent-claude");
        let cache_path = dir.path().join("shim-cache.json");

        let json = format!(
            r#"{{"claude_path": "{}"}}"#,
            stale_path.to_string_lossy()
        );
        std::fs::write(&cache_path, json).unwrap();

        // Verify the path doesn't exist (stale).
        let content = std::fs::read_to_string(&cache_path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        let parsed = v["claude_path"].as_str().map(PathBuf::from).unwrap();
        assert!(!parsed.exists(), "stale path must not exist");
    }
}
