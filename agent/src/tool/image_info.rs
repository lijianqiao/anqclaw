//! `image_info` tool — read image metadata and optionally base64-encode.
//!
//! Pure Rust implementation, no external crate needed.
//! Detects format via magic bytes, parses width/height from headers.

use anyhow::{bail, Result};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use super::Tool;

/// Maximum image file size: 10MB
const MAX_IMAGE_SIZE: u64 = 10 * 1024 * 1024;
/// Maximum file size for base64 encoding: 1MB
const MAX_BASE64_SIZE: u64 = 1_048_576;

pub struct ImageInfo {
    sandbox: PathBuf,
    blocked_dirs: Vec<String>,
    trusted_dirs: Vec<PathBuf>,
}

impl ImageInfo {
    pub fn new(
        file_access_dir: &str,
        blocked_dirs: Vec<String>,
        trusted_dirs: Vec<String>,
    ) -> Self {
        Self {
            sandbox: PathBuf::from(file_access_dir),
            blocked_dirs,
            trusted_dirs: trusted_dirs
                .into_iter()
                .map(|dir| crate::paths::resolve_configured_path(&dir))
                .filter_map(|dir| crate::paths::canonicalize_for_comparison(&dir).ok())
                .collect(),
        }
    }

    fn is_trusted(&self, path: &Path) -> bool {
        crate::paths::path_is_trusted(path, &self.trusted_dirs)
    }

    async fn do_execute(&self, args: serde_json::Value) -> Result<String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing `path` parameter"))?;

        let include_base64 = args
            .get("include_base64")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

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
            .map_err(|e| anyhow::anyhow!("stat `{}`: {e}", path.display()))?;
        let file_size = metadata.len();
        if file_size > MAX_IMAGE_SIZE {
            return Ok(format!(
                "[图片过大: {} bytes ({:.1} MB), 最大允许 10 MB]",
                file_size,
                file_size as f64 / 1_048_576.0
            ));
        }

        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| anyhow::anyhow!("read `{}`: {e}", path.display()))?;

        let (format, dimensions) = parse_image_info(&bytes)?;

        let mut result = format!(
            "格式: {}\n大小: {} bytes ({:.1} KB)",
            format,
            file_size,
            file_size as f64 / 1024.0
        );

        if let Some((w, h)) = dimensions {
            result.push_str(&format!("\n尺寸: {}x{} px", w, h));
        }

        result.push_str(&format!("\n路径: {}", path.display()));

        if include_base64 {
            if file_size > MAX_BASE64_SIZE {
                result.push_str(&format!(
                    "\n\n[base64 跳过: 图片 {:.1} MB 超过 1 MB 编码上限]",
                    file_size as f64 / 1_048_576.0
                ));
            } else {
                use base64::Engine;
                let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let mime = match format {
                    "PNG" => "image/png",
                    "JPEG" => "image/jpeg",
                    "GIF" => "image/gif",
                    "WebP" => "image/webp",
                    "BMP" => "image/bmp",
                    _ => "application/octet-stream",
                };
                result.push_str(&format!("\nMIME: {mime}"));
                result.push_str(&format!("\nbase64: {encoded}"));
            }
        }

        Ok(result)
    }
}

/// Parse image format and dimensions from raw bytes.
fn parse_image_info(bytes: &[u8]) -> Result<(&'static str, Option<(u32, u32)>)> {
    if bytes.len() < 4 {
        bail!("file too small to be a valid image");
    }

    // PNG: 8-byte signature + IHDR chunk at offset 16 (width: 4 bytes BE, height: 4 bytes BE)
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let dims = if bytes.len() >= 24 {
            let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
            let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
            Some((w, h))
        } else {
            None
        };
        return Ok(("PNG", dims));
    }

    // JPEG: search for SOF0/SOF2 marker (0xFF 0xC0 or 0xFF 0xC2)
    if bytes.starts_with(b"\xFF\xD8\xFF") {
        let dims = parse_jpeg_dimensions(bytes);
        return Ok(("JPEG", dims));
    }

    // GIF: width at offset 6 (2 bytes LE), height at offset 8 (2 bytes LE)
    if bytes.starts_with(b"GIF8") {
        let dims = if bytes.len() >= 10 {
            let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
            let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
            Some((w, h))
        } else {
            None
        };
        return Ok(("GIF", dims));
    }

    // WebP: RIFF....WEBP
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        let dims = parse_webp_dimensions(bytes);
        return Ok(("WebP", dims));
    }

    // BMP: width at offset 18 (4 bytes LE), height at offset 22 (4 bytes LE)
    if bytes.starts_with(b"BM") {
        let dims = if bytes.len() >= 26 {
            let w = u32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
            let h = u32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]);
            Some((w, h)) // BMP height is u32 here; negative (top-down) encoding is handled by i32 cast
        } else {
            None
        };
        return Ok(("BMP", dims));
    }

    bail!("unrecognized image format (no matching magic bytes)");
}

/// Parse JPEG dimensions by scanning for SOF markers.
fn parse_jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2; // Skip SOI marker (FF D8)
    while i + 1 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        // SOF0, SOF1, SOF2, SOF3
        if matches!(marker, 0xC0..=0xC3) && i + 9 < bytes.len() {
            let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
            return Some((w, h));
        }
        // Skip to next marker
        if i + 3 < bytes.len() {
            let seg_len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            i += 2 + seg_len;
        } else {
            break;
        }
    }
    None
}

/// Parse WebP dimensions (VP8/VP8L/VP8X).
fn parse_webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 30 {
        return None;
    }
    // VP8 lossy: "VP8 " at offset 12
    if &bytes[12..16] == b"VP8 " && bytes.len() >= 30 {
        let w = u16::from_le_bytes([bytes[26], bytes[27]]) as u32;
        let h = u16::from_le_bytes([bytes[28], bytes[29]]) as u32;
        return Some((w & 0x3FFF, h & 0x3FFF));
    }
    // VP8L lossless: "VP8L" at offset 12
    if &bytes[12..16] == b"VP8L" && bytes.len() >= 25 {
        let bits = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
        let w = (bits & 0x3FFF) + 1;
        let h = ((bits >> 14) & 0x3FFF) + 1;
        return Some((w, h));
    }
    // VP8X extended: width at 24 (3 bytes LE), height at 27 (3 bytes LE)
    if &bytes[12..16] == b"VP8X" && bytes.len() >= 30 {
        let w = (bytes[24] as u32) | ((bytes[25] as u32) << 8) | ((bytes[26] as u32) << 16);
        let h = (bytes[27] as u32) | ((bytes[28] as u32) << 8) | ((bytes[29] as u32) << 16);
        return Some((w + 1, h + 1));
    }
    None
}

impl Tool for ImageInfo {
    fn name(&self) -> &str {
        "image_info"
    }

    fn description(&self) -> &str {
        "Read image metadata (format, dimensions) and optionally return base64-encoded data. \
         Supports PNG, JPEG, GIF, WebP, and BMP."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Image file path (relative to workspace or absolute)"
                },
                "include_base64": {
                    "type": "boolean",
                    "description": "Whether to include base64-encoded image data (default: false, max 1MB)"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_png() {
        // Minimal PNG header + IHDR
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        // IHDR length (13 bytes)
        bytes.extend_from_slice(&[0, 0, 0, 13]);
        bytes.extend_from_slice(b"IHDR");
        // Width: 800, Height: 600
        bytes.extend_from_slice(&800u32.to_be_bytes());
        bytes.extend_from_slice(&600u32.to_be_bytes());
        // Padding
        bytes.extend_from_slice(&[8, 2, 0, 0, 0]);

        let (format, dims) = parse_image_info(&bytes).unwrap();
        assert_eq!(format, "PNG");
        assert_eq!(dims, Some((800, 600)));
    }

    #[test]
    fn test_detect_gif() {
        let mut bytes = b"GIF89a".to_vec();
        // Width: 320, Height: 240 (LE)
        bytes.extend_from_slice(&320u16.to_le_bytes());
        bytes.extend_from_slice(&240u16.to_le_bytes());

        let (format, dims) = parse_image_info(&bytes).unwrap();
        assert_eq!(format, "GIF");
        assert_eq!(dims, Some((320, 240)));
    }

    #[test]
    fn test_detect_bmp() {
        let mut bytes = vec![b'B', b'M'];
        bytes.extend_from_slice(&[0u8; 16]); // padding to offset 18
        // Width: 1024, Height: 768 (LE)
        bytes.extend_from_slice(&1024u32.to_le_bytes());
        bytes.extend_from_slice(&768u32.to_le_bytes());

        let (format, dims) = parse_image_info(&bytes).unwrap();
        assert_eq!(format, "BMP");
        assert_eq!(dims, Some((1024, 768)));
    }

    #[test]
    fn test_unknown_format() {
        let bytes = vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05];
        assert!(parse_image_info(&bytes).is_err());
    }
}
