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
    // For writes the file may not exist yet, so we canonicalize the highest
    // existing ancestor FIRST, verify it's inside the sandbox, then create
    // directories. This prevents symlink-based sandbox escapes.
    let canonical = if candidate.exists() {
        candidate.canonicalize()?
    } else {
        let file_name = candidate
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("path has no file name"))?;

        // Walk up to find the highest existing ancestor and canonicalize it.
        let parent = candidate
            .parent()
            .ok_or_else(|| anyhow::anyhow!("no parent directory for path"))?;
        let mut existing_ancestor = parent.to_path_buf();
        while !existing_ancestor.exists() {
            existing_ancestor = existing_ancestor
                .parent()
                .ok_or_else(|| anyhow::anyhow!("cannot resolve ancestor path"))?
                .to_path_buf();
        }
        let canonical_ancestor = existing_ancestor.canonicalize()?;

        // Verify the existing ancestor is inside the sandbox BEFORE creating dirs
        let sandbox_canonical = sandbox.canonicalize()?;
        if !canonical_ancestor.starts_with(&sandbox_canonical) {
            bail!(
                "path `{}` resolves outside the allowed directory `{}`",
                candidate.display(),
                sandbox_canonical.display()
            );
        }

        // Now safe to create directories
        std::fs::create_dir_all(parent)?;
        let canonical_parent = parent.canonicalize()?;
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

// ─── Binary format detection helpers ─────────────────────────────────────────

/// Detect image format by magic bytes. Returns format name or None.
fn detect_image_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 4 {
        return None;
    }
    if bytes.starts_with(b"\x89PNG") {
        Some("PNG")
    } else if bytes.starts_with(b"\xFF\xD8\xFF") {
        Some("JPEG")
    } else if bytes.starts_with(b"GIF8") {
        Some("GIF")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("WebP")
    } else if bytes.starts_with(b"BM") {
        Some("BMP")
    } else {
        None
    }
}

/// Detect known binary file formats by magic bytes. Returns format name or None.
fn detect_binary_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 4 {
        return None;
    }
    // ZIP / DOCX / XLSX / JAR (PK header)
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        return Some("ZIP/Archive");
    }
    // Windows executable
    if bytes.starts_with(b"MZ") {
        return Some("EXE/DLL");
    }
    // SQLite
    if bytes.starts_with(b"SQLite format 3") {
        return Some("SQLite");
    }
    // Gzip
    if bytes.starts_with(b"\x1f\x8b") {
        return Some("Gzip");
    }
    // ELF (Linux executable)
    if bytes.starts_with(b"\x7fELF") {
        return Some("ELF");
    }
    // Mach-O (macOS executable)
    if bytes.len() >= 4 {
        let magic = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if magic == 0xFEEDFACE || magic == 0xFEEDFACF || magic == 0xCAFEBABE {
            return Some("Mach-O");
        }
    }
    // tar
    if bytes.len() >= 262 && &bytes[257..262] == b"ustar" {
        return Some("tar");
    }
    // 7z
    if bytes.starts_with(b"7z\xBC\xAF\x27\x1C") {
        return Some("7z");
    }
    // RAR
    if bytes.starts_with(b"Rar!\x1a\x07") {
        return Some("RAR");
    }
    None
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

    /// Maximum file size for file_read (20MB)
    const FILE_READ_MAX_SIZE: u64 = 20 * 1024 * 1024;
    /// Maximum chars for lossy fallback output
    const LOSSY_MAX_CHARS: usize = 2000;

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

        // Step 0: File size pre-check
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|e| anyhow::anyhow!("stat `{}`: {e}", path.display()))?;
        let file_size = metadata.len();
        if file_size > Self::FILE_READ_MAX_SIZE {
            return Ok(format!(
                "[文件过大: {} bytes ({:.1} MB), 最大允许 {} MB。请使用专用工具处理。]",
                file_size,
                file_size as f64 / 1_048_576.0,
                Self::FILE_READ_MAX_SIZE / 1_048_576
            ));
        }

        // Step 1: Read raw bytes once (avoids double-read of read_to_string then read)
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| anyhow::anyhow!("read `{}`: {e}", path.display()))?;

        // Step 2: Try UTF-8 conversion (zero-copy if valid)
        match String::from_utf8(bytes) {
            Ok(content) => return Ok(content),
            Err(e) => {
                // Reclaim the bytes for binary detection
                let bytes = e.into_bytes();

                // Step 3: PDF detection
                if bytes.starts_with(b"%PDF-") {
                    return Ok(format!(
                        "[PDF 文件: {} bytes ({:.1} MB)。请使用 `pdf_read` 工具提取文本内容。路径: {}]",
                        file_size,
                        file_size as f64 / 1_048_576.0,
                        path.display()
                    ));
                }

                // Step 4: Image detection
                if let Some(format) = detect_image_format(&bytes) {
                    return Ok(format!(
                        "[图片文件: {format}, {} bytes ({:.1} KB)。请使用 `image_info` 工具查看详情。路径: {}]",
                        file_size,
                        file_size as f64 / 1024.0,
                        path.display()
                    ));
                }

                // Step 5: Known binary format detection
                if let Some(format) = detect_binary_format(&bytes) {
                    return Ok(format!(
                        "[二进制文件: {format}, {} bytes ({:.1} KB), 无法直接读取]",
                        file_size,
                        file_size as f64 / 1024.0
                    ));
                }

                // Step 6: Lossy fallback with truncation
                let lossy = String::from_utf8_lossy(&bytes);
                if lossy.len() > Self::LOSSY_MAX_CHARS {
                    Ok(format!(
                        "[警告: 非纯文本文件，已截断到前 {} 字符]\n\n{}",
                        Self::LOSSY_MAX_CHARS,
                        &lossy[..Self::LOSSY_MAX_CHARS]
                    ))
                } else {
                    Ok(lossy.into_owned())
                }
            }
        }
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

/// Public wrapper for resolve_safe_path (used by pdf_read and image_info tools)
pub fn resolve_safe_path_pub(sandbox: &Path, user_path: &str) -> Result<PathBuf> {
    resolve_safe_path(sandbox, user_path)
}

/// Public wrapper for check_blocked_dirs (used by pdf_read and image_info tools)
pub fn check_blocked_dirs_pub(path: &Path, blocked_dirs: &[String]) -> Result<()> {
    check_blocked_dirs(path, blocked_dirs)
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
