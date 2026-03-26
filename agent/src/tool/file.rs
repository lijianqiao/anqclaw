//! `file_read` and `file_write` tools.
//!
//! Both tools enforce a **sandbox directory** (`file_access_dir` from config).
//! Paths are canonicalised and must start with the sandbox prefix; otherwise
//! the operation is rejected to prevent path-traversal attacks.

use anyhow::{bail, Result};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use super::Tool;

// ─── Shared helper ───────────────────────────────────────────────────────────

/// Resolves `user_path` relative to `sandbox` and ensures the result is inside
/// the sandbox after canonicalisation.
///
/// Returns the canonical absolute path on success.
fn resolve_safe_path(sandbox: &Path, user_path: &str) -> Result<PathBuf> {
    let candidate = if Path::new(user_path).is_absolute() {
        PathBuf::from(user_path)
    } else {
        sandbox.join(user_path)
    };

    // For reads the file must exist so we can canonicalize directly.
    // For writes the file may not exist yet, so we canonicalize the *parent*.
    let canonical = if candidate.exists() {
        candidate.canonicalize()?
    } else {
        let parent = candidate
            .parent()
            .ok_or_else(|| anyhow::anyhow!("no parent directory for path"))?;

        // Create parent dirs if needed (write scenario)
        std::fs::create_dir_all(parent)?;
        let canonical_parent = parent.canonicalize()?;
        let file_name = candidate
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("path has no file name"))?;
        canonical_parent.join(file_name)
    };

    let sandbox_canonical = sandbox.canonicalize()?;

    if !canonical.starts_with(&sandbox_canonical) {
        bail!(
            "path `{}` is outside the allowed directory `{}`",
            canonical.display(),
            sandbox_canonical.display()
        );
    }

    Ok(canonical)
}

// ─── Blocked directory check ─────────────────────────────────────────────────

fn check_blocked_dirs(path: &Path, blocked_dirs: &[String]) -> Result<()> {
    let path_str = path.to_string_lossy();
    for dir in blocked_dirs {
        if path_str.contains(dir.as_str()) {
            bail!(
                "path `{}` references blocked directory: {}",
                path.display(),
                dir
            );
        }
    }
    Ok(())
}

// ─── file_read ───────────────────────────────────────────────────────────────

pub struct FileRead {
    sandbox: PathBuf,
    blocked_dirs: Vec<String>,
    trusted_dirs: Vec<String>,
}

impl FileRead {
    pub fn new(file_access_dir: &str, blocked_dirs: Vec<String>, trusted_dirs: Vec<String>) -> Self {
        Self {
            sandbox: PathBuf::from(file_access_dir),
            blocked_dirs,
            trusted_dirs,
        }
    }

    fn is_trusted(&self, path: &Path) -> bool {
        let s = path.to_string_lossy();
        self.trusted_dirs.iter().any(|d| s.starts_with(d.as_str()))
    }

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `path` parameter"))?;

        let path = resolve_safe_path(&self.sandbox, path_str);
        // If resolve_safe_path fails (outside sandbox), check if it's in a trusted dir
        let path = match path {
            Ok(p) => p,
            Err(_) => {
                let abs = if Path::new(path_str).is_absolute() {
                    PathBuf::from(path_str)
                } else {
                    self.sandbox.join(path_str)
                };
                if self.is_trusted(&abs) {
                    abs
                } else {
                    // Re-call to get the original error
                    resolve_safe_path(&self.sandbox, path_str)?
                }
            }
        };
        check_blocked_dirs(&path, &self.blocked_dirs)?;

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| anyhow::anyhow!("read `{}`: {e}", path.display()))?;

        Ok(content)
    }
}

impl Tool for FileRead {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. The path must be inside the workspace directory."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path (relative to workspace or absolute)"
                }
            },
            "required": ["path"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(self.do_execute(args))
    }
}

// ─── file_write ──────────────────────────────────────────────────────────────

pub struct FileWrite {
    sandbox: PathBuf,
    blocked_dirs: Vec<String>,
    trusted_dirs: Vec<String>,
}

impl FileWrite {
    pub fn new(file_access_dir: &str, blocked_dirs: Vec<String>, trusted_dirs: Vec<String>) -> Self {
        Self {
            sandbox: PathBuf::from(file_access_dir),
            blocked_dirs,
            trusted_dirs,
        }
    }

    fn is_trusted(&self, path: &Path) -> bool {
        let s = path.to_string_lossy();
        self.trusted_dirs.iter().any(|d| s.starts_with(d.as_str()))
    }

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `path` parameter"))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `content` parameter"))?;

        let path = resolve_safe_path(&self.sandbox, path_str);
        let path = match path {
            Ok(p) => p,
            Err(_) => {
                let abs = if Path::new(path_str).is_absolute() {
                    PathBuf::from(path_str)
                } else {
                    self.sandbox.join(path_str)
                };
                if self.is_trusted(&abs) {
                    abs
                } else {
                    resolve_safe_path(&self.sandbox, path_str)?
                }
            }
        };
        check_blocked_dirs(&path, &self.blocked_dirs)?;

        tokio::fs::write(&path, content)
            .await
            .map_err(|e| anyhow::anyhow!("write `{}`: {e}", path.display()))?;

        Ok(format!("Written {} bytes to {}", content.len(), path.display()))
    }
}

impl Tool for FileWrite {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write content to a file. The path must be inside the workspace directory. Parent directories are created automatically."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path (relative to workspace or absolute)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(self.do_execute(args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_safe_path_inside_sandbox() {
        let dir = std::env::temp_dir().join("anqclaw_test_file_sandbox");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("test.txt"), "hello").unwrap();

        let result = resolve_safe_path(&dir, "test.txt");
        assert!(result.is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_safe_path_traversal_blocked() {
        let dir = std::env::temp_dir().join("anqclaw_test_file_traversal");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let result = resolve_safe_path(&dir, "../../etc/passwd");
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_check_blocked_dirs() {
        let path = Path::new("/home/user/.ssh/id_rsa");
        let blocked = vec![".ssh".to_string()];
        let result = check_blocked_dirs(path, &blocked);
        assert!(result.is_err());
    }

    #[test]
    fn test_check_blocked_dirs_ok() {
        let path = Path::new("/home/user/docs/readme.md");
        let blocked = vec![".ssh".to_string()];
        let result = check_blocked_dirs(path, &blocked);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_file_read_write_roundtrip() {
        let dir = std::env::temp_dir().join("anqclaw_test_file_rw");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let writer = FileWrite::new(dir.to_str().unwrap(), vec![], vec![]);
        let args = serde_json::json!({
            "path": "test_rw.txt",
            "content": "hello world"
        });
        let r = writer.do_execute(args).await;
        assert!(r.is_ok());

        let reader = FileRead::new(dir.to_str().unwrap(), vec![], vec![]);
        let args = serde_json::json!({ "path": "test_rw.txt" });
        let content = reader.do_execute(args).await.unwrap();
        assert_eq!(content, "hello world");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_file_read_missing_path_param() {
        let reader = FileRead::new("/tmp", vec![], vec![]);
        let args = serde_json::json!({});
        let result = reader.do_execute(args).await;
        assert!(result.is_err());
    }
}
