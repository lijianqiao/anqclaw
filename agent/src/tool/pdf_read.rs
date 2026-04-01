//! `pdf_read` tool — extract text from PDF files.
//!
//! Uses `pdf_extract` crate when the `rag-pdf` feature is enabled.
//! Falls back to a clear error message when the feature is not compiled in.

use anyhow::Result;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use super::Tool;

/// Maximum PDF file size: 50MB
const MAX_PDF_SIZE: u64 = 50 * 1024 * 1024;

pub struct PdfRead {
    sandbox: PathBuf,
    blocked_dirs: Vec<String>,
    trusted_dirs: Vec<PathBuf>,
    max_chars: usize,
}

impl PdfRead {
    pub fn new(
        file_access_dir: &str,
        blocked_dirs: Vec<String>,
        trusted_dirs: Vec<String>,
        max_chars: u32,
    ) -> Self {
        Self {
            sandbox: PathBuf::from(file_access_dir),
            blocked_dirs,
            trusted_dirs: trusted_dirs
                .into_iter()
                .map(|dir| crate::paths::resolve_configured_path(&dir))
                .filter_map(|dir| crate::paths::canonicalize_for_comparison(&dir).ok())
                .collect(),
            max_chars: max_chars as usize,
        }
    }

    fn is_trusted(&self, path: &Path) -> bool {
        crate::paths::path_is_trusted(path, &self.trusted_dirs)
    }

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `path` parameter / 缺少 `path` 参数"))?;

        #[allow(unused_variables)]
        let max_chars = args
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .map(|v| v.min(200_000) as usize)
            .unwrap_or(self.max_chars);

        let path = super::file::resolve_safe_path_pub(&self.sandbox, path_str);
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
                    super::file::resolve_safe_path_pub(&self.sandbox, path_str)?
                }
            }
        };
        super::file::check_blocked_dirs_pub(&path, &self.blocked_dirs)?;

        // File size check
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|e| anyhow::anyhow!("stat `{}`: {e} / 获取文件信息失败", path.display()))?;
        if metadata.len() > MAX_PDF_SIZE {
            return Ok(format!(
                "[PDF 过大: {} bytes ({:.1} MB), 最大允许 50 MB]",
                metadata.len(),
                metadata.len() as f64 / 1_048_576.0
            ));
        }

        // Verify it's actually a PDF
        let header = tokio::fs::read(&path)
            .await
            .map_err(|e| anyhow::anyhow!("read `{}`: {e} / 读取文件失败", path.display()))?;
        if !header.starts_with(b"%PDF-") {
            anyhow::bail!(
                "file `{}` is not a valid PDF (missing %PDF- header) / 文件不是有效的 PDF（缺少 %PDF- 头）",
                path.display()
            );
        }

        // Extract text using pdf_extract (CPU-intensive, use spawn_blocking)
        #[cfg(feature = "rag-pdf")]
        {
            let path_clone = path.clone();
            let text = tokio::task::spawn_blocking(move || -> Result<String> {
                let bytes = std::fs::read(&path_clone).map_err(|e| {
                    anyhow::anyhow!("read `{}`: {e} / 读取文件失败", path_clone.display())
                })?;
                pdf_extract::extract_text_from_mem(&bytes)
                    .map_err(|e| anyhow::anyhow!("PDF extraction failed / PDF 提取失败: {e}"))
            })
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking failed / 后台任务失败: {e}"))??;

            if text.trim().is_empty() {
                return Ok(
                    "[PDF 提取完成，但未包含可提取的文本内容（可能是扫描件/图片 PDF）]".to_string(),
                );
            }

            if text.len() > max_chars {
                Ok(format!(
                    "[PDF 内容已截断: 显示前 {} / {} 字符]\n\n{}",
                    max_chars,
                    text.len(),
                    &text[..max_chars]
                ))
            } else {
                Ok(text)
            }
        }

        #[cfg(not(feature = "rag-pdf"))]
        {
            Ok(format!(
                "[PDF 提取功能未启用。请使用 `cargo build --features rag-pdf` 重新编译以启用 PDF 文本提取。\n\
                 文件: {}, 大小: {} bytes ({:.1} MB)]",
                path.display(),
                metadata.len(),
                metadata.len() as f64 / 1_048_576.0
            ))
        }
    }
}

impl Tool for PdfRead {
    fn name(&self) -> &str {
        "pdf_read"
    }

    fn description(&self) -> &str {
        "Extract text content from a PDF file. Supports max_chars to limit output length."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "PDF file path (relative to workspace or absolute)"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to return (default: 50000, max: 200000)"
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
