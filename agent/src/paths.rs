//! Centralized path resolution for anqclaw.
//!
//! All user-facing paths (config, workspace, database, logs) resolve relative to
//! `~/.anqclaw/` unless an absolute path is given.

use std::path::{Path, PathBuf};

/// Returns the anqclaw home directory: `~/.anqclaw/`
///
/// Platform-specific:
/// - Windows: `C:\Users\<user>\.anqclaw`
/// - macOS:   `/Users/<user>/.anqclaw`
/// - Linux:   `/home/<user>/.anqclaw`
pub fn anqclaw_home() -> PathBuf {
    dirs::home_dir()
        .expect("cannot determine home directory")
        .join(".anqclaw")
}

/// Resolves a path relative to `base`.
///
/// - If `relative` is absolute, returns it as-is.
/// - Otherwise, joins `base` / `relative`.
pub fn resolve_path(base: &Path, relative: &str) -> PathBuf {
    // Strip leading "./" or ".\" — common in config files but creates ugly joined paths
    let cleaned = relative
        .strip_prefix("./")
        .or_else(|| relative.strip_prefix(".\\"))
        .unwrap_or(relative);

    let p = Path::new(cleaned);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(cleaned)
    }
}

/// Resolves a configured path relative to `~/.anqclaw/` and supports `~/...`.
pub fn resolve_configured_path(relative: &str) -> PathBuf {
    if relative == "~" {
        return dirs::home_dir().expect("cannot determine home directory");
    }
    if let Some(rest) = relative
        .strip_prefix("~/")
        .or_else(|| relative.strip_prefix("~\\"))
    {
        return dirs::home_dir()
            .expect("cannot determine home directory")
            .join(rest);
    }

    resolve_path(&anqclaw_home(), relative)
}

/// Canonicalizes an existing path, or its highest existing ancestor plus the remaining suffix.
pub fn canonicalize_for_comparison(path: &Path) -> std::io::Result<PathBuf> {
    if path.exists() {
        return path.canonicalize();
    }

    let mut current = path.to_path_buf();
    let mut suffix = Vec::new();

    while !current.exists() {
        if let Some(name) = current.file_name() {
            suffix.push(PathBuf::from(name));
        }
        current = current
            .parent()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("cannot resolve existing ancestor for {}", path.display()),
                )
            })?
            .to_path_buf();
    }

    let mut canonical = current.canonicalize()?;
    for component in suffix.iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

/// Returns true if `path` resolves within any configured trusted root.
pub fn path_is_trusted(path: &Path, trusted_roots: &[PathBuf]) -> bool {
    let Ok(candidate) = canonicalize_for_comparison(path) else {
        return false;
    };

    trusted_roots.iter().any(|root| candidate.starts_with(root))
}

/// Ensures the standard subdirectory structure exists under `home`.
///
/// Creates: workspace/, data/, sessions/, skills/, logs/
pub fn ensure_dirs(home: &Path) -> std::io::Result<()> {
    for sub in &["workspace", "data", "sessions", "skills", "logs"] {
        std::fs::create_dir_all(home.join(sub))?;
    }
    Ok(())
}

/// Searches for a config file in priority order:
///
/// 1. `--config <path>` CLI argument (highest priority)
/// 2. `$ANQCLAW_CONFIG` environment variable
/// 3. `./config.toml` (current working directory — handy for development)
/// 4. `~/.anqclaw/config.toml` (standard production location)
///
/// Returns `None` if no config file is found at any location.
pub fn find_config(cli_path: Option<&str>) -> Option<PathBuf> {
    // Priority 1: explicit CLI argument
    if let Some(p) = cli_path {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
        // If the user specified a path but it doesn't exist, still return it
        // so we get a clear error message later during load
        return Some(path);
    }

    // Priority 2: environment variable
    if let Ok(env_path) = std::env::var("ANQCLAW_CONFIG") {
        let path = PathBuf::from(&env_path);
        if path.exists() {
            return Some(path);
        }
    }

    // Priority 3: current working directory
    let cwd_config = PathBuf::from("config.toml");
    if cwd_config.exists() {
        return Some(cwd_config);
    }

    // Priority 4: ~/.anqclaw/config.toml
    let home_config = anqclaw_home().join("config.toml");
    if home_config.exists() {
        return Some(home_config);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_path_absolute() {
        let base = Path::new("/home/user/.anqclaw");
        // Absolute path stays absolute
        if cfg!(windows) {
            let resolved = resolve_path(base, "C:\\some\\abs\\path");
            assert!(resolved.is_absolute());
        } else {
            let resolved = resolve_path(base, "/some/abs/path");
            assert_eq!(resolved, PathBuf::from("/some/abs/path"));
        }
    }

    #[test]
    fn test_resolve_path_relative() {
        let base = Path::new("/home/user/.anqclaw");
        let resolved = resolve_path(base, "data/memory.db");
        assert_eq!(
            resolved,
            PathBuf::from("/home/user/.anqclaw/data/memory.db")
        );
    }

    #[test]
    fn test_anqclaw_home_not_empty() {
        let home = anqclaw_home();
        assert!(home.ends_with(".anqclaw"));
    }

    #[test]
    fn test_resolve_configured_path_tilde() {
        let resolved = resolve_configured_path("~/projects");
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("projects"));
    }
}
