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

// ─── file_read ───────────────────────────────────────────────────────────────

pub struct FileRead {
    sandbox: PathBuf,
}

impl FileRead {
    pub fn new(file_access_dir: &str) -> Self {
        Self {
            sandbox: PathBuf::from(file_access_dir),
        }
    }

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `path` parameter"))?;

        let path = resolve_safe_path(&self.sandbox, path_str)?;

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
}

impl FileWrite {
    pub fn new(file_access_dir: &str) -> Self {
        Self {
            sandbox: PathBuf::from(file_access_dir),
        }
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

        let path = resolve_safe_path(&self.sandbox, path_str)?;

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
