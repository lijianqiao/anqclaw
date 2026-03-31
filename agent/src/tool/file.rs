//! `file_read` and `file_write` tools.
//!
//! Both tools enforce a **sandbox directory** (`file_access_dir` from config).
//! Paths are canonicalised and must start with the sandbox prefix; otherwise
//! the operation is rejected to prevent path-traversal attacks.

use anyhow::{Result, bail};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use super::Tool;

// ─── Shared helper ───────────────────────────────────────────────────────────

struct PathAccessPolicy {
    sandbox: PathBuf,
    blocked_dirs: Vec<String>,
    trusted_dirs: Vec<PathBuf>,
}

impl PathAccessPolicy {
    fn new(file_access_dir: &str, blocked_dirs: Vec<String>, trusted_dirs: Vec<String>) -> Self {
        Self {
            sandbox: PathBuf::from(file_access_dir),
            blocked_dirs,
            trusted_dirs: prepare_trusted_dirs(trusted_dirs),
        }
    }

    fn resolve_tool_path(&self, user_path: &str) -> Result<PathBuf> {
        let normalized_path = normalize_tool_path(user_path);
        resolve_tool_path(&self.sandbox, &self.trusted_dirs, &normalized_path)
    }

    fn check_blocked(&self, path: &Path) -> Result<()> {
        check_blocked_dirs(path, &self.blocked_dirs)
    }
}

fn prepare_trusted_dirs(trusted_dirs: Vec<String>) -> Vec<PathBuf> {
    trusted_dirs
        .into_iter()
        .map(|dir| crate::paths::resolve_configured_path(&dir))
        .filter_map(|dir| crate::paths::canonicalize_for_comparison(&dir).ok())
        .collect()
}

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
            .ok_or_else(|| anyhow::anyhow!("path has no file name / 路径没有文件名"))?;

        // Walk up to find the highest existing ancestor and canonicalize it.
        let parent = candidate
            .parent()
            .ok_or_else(|| anyhow::anyhow!("no parent directory for path / 路径没有父目录"))?;
        let mut existing_ancestor = parent.to_path_buf();
        while !existing_ancestor.exists() {
            existing_ancestor = existing_ancestor
                .parent()
                .ok_or_else(|| anyhow::anyhow!("cannot resolve ancestor path / 无法解析祖先路径"))?
                .to_path_buf();
        }
        let canonical_ancestor = existing_ancestor.canonicalize()?;

        // Verify the existing ancestor is inside the sandbox BEFORE creating dirs
        let sandbox_canonical = sandbox.canonicalize()?;
        if !canonical_ancestor.starts_with(&sandbox_canonical) {
            bail!(
                "path `{}` resolves outside the allowed directory `{}` / 路径 `{}` 解析到允许目录 `{}` 之外",
                candidate.display(),
                sandbox_canonical.display(),
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
            "path `{}` is outside the allowed directory `{}` / 路径 `{}` 在允许目录 `{}` 之外",
            canonical.display(),
            sandbox_canonical.display(),
            canonical.display(),
            sandbox_canonical.display()
        );
    }

    Ok(canonical)
}

fn resolve_tool_path(
    sandbox: &Path,
    trusted_dirs: &[PathBuf],
    normalized_path: &str,
) -> Result<PathBuf> {
    let path = resolve_safe_path(sandbox, normalized_path);
    match path {
        Ok(path) => Ok(path),
        Err(_) => {
            let abs = if Path::new(normalized_path).is_absolute() {
                PathBuf::from(normalized_path)
            } else {
                sandbox.join(normalized_path)
            };
            if crate::paths::path_is_trusted(&abs, trusted_dirs) {
                Ok(abs)
            } else {
                resolve_safe_path(sandbox, normalized_path)
            }
        }
    }
}

fn is_script_like_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "py" | "ps1" | "js" | "ts" | "mjs" | "cjs" | "sh" | "bat" | "cmd" | "rb"
            )
        })
        .unwrap_or(false)
}

fn normalize_tool_path(user_path: &str) -> String {
    let trimmed = user_path.trim();
    if Path::new(trimmed).is_absolute() {
        return trimmed.to_string();
    }

    let normalized = trimmed.replace('\\', "/");
    if let Some(rest) = normalized.strip_prefix("workspace/") {
        let rest = rest.trim_start_matches('/');
        if rest.starts_with("script/") {
            return rest.to_string();
        }
        if is_script_like_path(rest) {
            return format!("script/{rest}");
        }
        return rest.to_string();
    }

    normalized
}

// ─── Blocked directory check ─────────────────────────────────────────────────

fn check_blocked_dirs(path: &Path, blocked_dirs: &[String]) -> Result<()> {
    let path_str = path.to_string_lossy();
    for dir in blocked_dirs {
        if path_str.contains(dir.as_str()) {
            bail!(
                "path `{}` references blocked directory: {} / 路径 `{}` 引用了被屏蔽的目录: {}",
                path.display(),
                dir,
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
    policy: PathAccessPolicy,
}

impl FileRead {
    pub fn new(
        file_access_dir: &str,
        blocked_dirs: Vec<String>,
        trusted_dirs: Vec<String>,
    ) -> Self {
        Self {
            policy: PathAccessPolicy::new(file_access_dir, blocked_dirs, trusted_dirs),
        }
    }

    /// Maximum file size for file_read (20MB)
    const FILE_READ_MAX_SIZE: u64 = 20 * 1024 * 1024;
    /// Maximum chars for lossy fallback output
    const LOSSY_MAX_CHARS: usize = 2000;

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `path` parameter / 缺少 `path` 参数"))?;

        let path = self.policy.resolve_tool_path(path_str)?;
        self.policy.check_blocked(&path)?;

        // Step 0: File size pre-check
        let metadata = tokio::fs::metadata(&path).await.map_err(|e| {
            anyhow::anyhow!(
                "stat `{}`: {e} / 获取文件信息 `{}` 失败: {e}",
                path.display(),
                path.display()
            )
        })?;
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
        let bytes = tokio::fs::read(&path).await.map_err(|e| {
            anyhow::anyhow!(
                "read `{}`: {e} / 读取文件 `{}` 失败: {e}",
                path.display(),
                path.display()
            )
        })?;

        // Step 2: Try UTF-8 conversion (zero-copy if valid)
        match String::from_utf8(bytes) {
            Ok(content) => Ok(content),
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
        "Read the contents of a file. The path must be inside the workspace directory. Use paths relative to the workspace root; generated scripts should usually live under script/. / 读取文件内容。路径必须在工作区目录内。使用相对于工作区根目录的路径；生成的脚本通常应该放在 script/ 目录下。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path (relative to workspace or absolute) / 文件路径（相对于工作区或绝对路径）"
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
    policy: PathAccessPolicy,
}

impl FileWrite {
    pub fn new(
        file_access_dir: &str,
        blocked_dirs: Vec<String>,
        trusted_dirs: Vec<String>,
    ) -> Self {
        Self {
            policy: PathAccessPolicy::new(file_access_dir, blocked_dirs, trusted_dirs),
        }
    }

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `path` parameter / 缺少 `path` 参数"))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `content` parameter / 缺少 `content` 参数"))?;

        let path = self.policy.resolve_tool_path(path_str)?;
        self.policy.check_blocked(&path)?;

        tokio::fs::write(&path, content).await.map_err(|e| {
            anyhow::anyhow!(
                "write `{}`: {e} / 写入文件 `{}` 失败: {e}",
                path.display(),
                path.display()
            )
        })?;

        Ok(format!(
            "Written {} bytes to {} / 已写入 {} 字节到 {}",
            content.len(),
            path.display(),
            content.len(),
            path.display()
        ))
    }
}

impl Tool for FileWrite {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write content to a file. The path must be inside the workspace directory. Use paths relative to the workspace root; generated scripts should usually live under script/. Parent directories are created automatically. / 将内容写入文件。路径必须在工作区目录内。使用相对于工作区根目录的路径；生成的脚本通常应位于 script/ 下。父目录会自动创建。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path (relative to workspace or absolute) / 文件路径（相对于工作区或绝对路径）"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file / 要写入文件的内容"
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

    #[tokio::test]
    async fn test_file_write_workspace_script_prefix_maps_to_script_dir() {
        let dir = std::env::temp_dir().join("anqclaw_test_file_script_dir");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let writer = FileWrite::new(dir.to_str().unwrap(), vec![], vec![]);
        let args = serde_json::json!({
            "path": "workspace/stats_devices.py",
            "content": "print('ok')"
        });
        writer.do_execute(args).await.unwrap();

        assert!(dir.join("script").join("stats_devices.py").exists());
        assert!(!dir.join("workspace").join("stats_devices.py").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_trusted_dir_requires_real_path_boundary() {
        let dir = std::env::temp_dir().join("anqclaw_test_trusted_boundary");
        let trusted = dir.join("trusted");
        let sibling = dir.join("trusted-other");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&trusted).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        let reader = FileRead::new(
            dir.to_str().unwrap(),
            vec![],
            vec![trusted.to_string_lossy().to_string()],
        );

        assert!(crate::paths::path_is_trusted(
            &trusted.join("ok.txt"),
            &reader.policy.trusted_dirs
        ));
        assert!(!crate::paths::path_is_trusted(
            &sibling.join("not-ok.txt"),
            &reader.policy.trusted_dirs
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
