use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::LazyLock;

use async_trait::async_trait;
use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use zen_core::sandbox::SandboxValidator;

#[derive(Clone)]
pub struct FsReadTool {
    validator: SandboxValidator,
}

const NAME: &str = "fs.read";
const DESCRIPTION: &str =
    "Read file contents with optional line offset/limit or byte-range offset/length";

static ARGS_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Absolute or workspace-relative file path" },
            "offset": { "type": "integer", "description": "Starting line number (1-based, default 1)" },
            "limit": { "type": "integer", "description": "Maximum lines to read (default 2000)" },
            "offset_bytes": { "type": "integer", "description": "Byte offset to start reading from (default 0)" },
            "length": { "type": "integer", "description": "Maximum bytes to return" },
            "max_bytes": { "type": "integer", "description": "Hard ceiling on bytes read into memory (default 1048576); if the file is larger, only this many bytes are read and truncated is set" },
            "encoding": { "type": "string", "description": "Output encoding: 'utf8' (default) or 'base64' (for binary files)" }
        },
        "required": ["path"]
    })
});

static RESULT_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "content": { "type": "string" },
            "lines_read": { "type": "integer" },
            "total_lines": { "type": "integer" },
            "truncated": { "type": "boolean" },
            "is_binary": { "type": "boolean" },
            "binary": { "type": "boolean" },
            "bytes_read": { "type": "integer" }
        }
    })
});

impl FsReadTool {
    pub fn new(validator: SandboxValidator) -> Self {
        Self { validator }
    }
}

const DEFAULT_LIMIT: usize = 2000;
const DEFAULT_MAX_BYTES: usize = 1_048_576;

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF16BE_BOM: &[u8] = &[0xFE, 0xFF];

/// Returns a label for a leading byte-order mark, if any.
fn detect_bom(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(UTF8_BOM) {
        Some("utf-8")
    } else if data.starts_with(UTF16LE_BOM) {
        Some("utf-16le")
    } else if data.starts_with(UTF16BE_BOM) {
        Some("utf-16be")
    } else {
        None
    }
}

/// Binary detection: any NUL byte in the read portion marks the file binary.
/// UTF-16 BOMs imply binary content even before NUL scanning.
fn is_binary(data: &[u8]) -> bool {
    if matches!(detect_bom(data), Some("utf-16le") | Some("utf-16be")) {
        return true;
    }
    data.contains(&0x00)
}

/// Strip a leading UTF-8 BOM from decoded text.
fn strip_utf8_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// Read until `buf` is full or EOF, returning the number of bytes read.
async fn read_up_to(file: &mut tokio::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = file.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

#[async_trait]
impl Tool for FsReadTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: DESCRIPTION.to_string(),
            args_schema: ARGS_SCHEMA.clone(),
            result_schema: RESULT_SCHEMA.clone(),
        }
    }

    async fn invoke(&self, args: Value) -> Result<Value, KernelError> {
        let path_str = args["path"].as_str().ok_or_else(|| {
            KernelError::InvalidArgument("Missing or invalid 'path' field".into())
        })?;

        let offset = args.get("offset").and_then(|v| v.as_i64()).unwrap_or(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(DEFAULT_LIMIT as i64) as usize;
        let offset_bytes = args
            .get("offset_bytes")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0) as usize;
        let length = args
            .get("length")
            .and_then(|v| v.as_i64())
            .filter(|&v| v > 0)
            .map(|v| v as usize);
        let max_bytes = args
            .get("max_bytes")
            .and_then(|v| v.as_i64())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_MAX_BYTES as i64) as usize;
        let encoding = args
            .get("encoding")
            .and_then(|v| v.as_str())
            .unwrap_or("utf8");
        let is_base64 = encoding.eq_ignore_ascii_case("base64");

        let path = PathBuf::from(path_str);

        self.validator
            .validate_path_for_read(&path)
            .map_err(KernelError::ToolFailed)?;

        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|e| KernelError::ToolFailed(format!("Failed to read {}: {}", path_str, e)))?;
        let file_size = metadata.len() as usize;

        let byte_mode = args.get("offset_bytes").is_some() || args.get("length").is_some();

        // Read a bounded portion of the file into memory (never more than max_bytes).
        let (bytes, truncated_by_size) = if byte_mode {
            let start = offset_bytes.min(file_size);
            let want = length.map(|l| l.min(max_bytes)).unwrap_or(max_bytes);
            let to_read = want.min(file_size - start);
            let mut file = tokio::fs::File::open(&path).await.map_err(|e| {
                KernelError::ToolFailed(format!("Failed to open {}: {}", path_str, e))
            })?;
            file.seek(SeekFrom::Start(start as u64))
                .await
                .map_err(|e| {
                    KernelError::ToolFailed(format!("Failed to seek {}: {}", path_str, e))
                })?;
            let mut buf = vec![0u8; to_read];
            let n = read_up_to(&mut file, &mut buf).await.map_err(|e| {
                KernelError::ToolFailed(format!("Failed to read {}: {}", path_str, e))
            })?;
            buf.truncate(n);
            (buf, file_size > max_bytes)
        } else {
            // Bounded read in ALL branches: a concurrent writer could grow
            // the file past max_bytes between the metadata() check above and
            // the read, so an unbounded tokio::fs::read here was an OOM
            // vector. Reading max_bytes + 1 lets us detect post-read growth
            // (truncated) instead of silently believing the stale metadata.
            let mut file = tokio::fs::File::open(&path).await.map_err(|e| {
                KernelError::ToolFailed(format!("Failed to open {}: {}", path_str, e))
            })?;
            let read_cap = max_bytes.saturating_add(1);
            let mut buf = vec![0u8; read_cap];
            let n = read_up_to(&mut file, &mut buf).await.map_err(|e| {
                KernelError::ToolFailed(format!("Failed to read {}: {}", path_str, e))
            })?;
            let truncated = n > max_bytes;
            buf.truncate(n.min(max_bytes));
            (buf, truncated)
        };
        let bytes_read = bytes.len();
        let mut truncated =
            truncated_by_size || (byte_mode && offset_bytes + bytes_read < file_size);

        if is_binary(&bytes) {
            if is_base64 {
                use base64::Engine;
                let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                return Ok(json!({
                    "path": path_str,
                    "content": encoded,
                    "lines_read": 0,
                    "total_lines": 0,
                    "truncated": truncated,
                    "is_binary": true,
                    "binary": true,
                    "bytes_read": bytes_read
                }));
            }
            return Ok(json!({
                "path": path_str,
                "content": "",
                "lines_read": 0,
                "total_lines": 0,
                "truncated": truncated,
                "is_binary": true,
                "bytes_read": bytes_read
            }));
        }

        let text = String::from_utf8_lossy(&bytes);
        let text = strip_utf8_bom(&text);
        let all_lines: Vec<&str> = text.lines().collect();
        let total_lines = all_lines.len();

        let (selected, lines_read, line_truncated) = if byte_mode {
            // Byte ranges are returned as-is; line counts describe the portion read.
            (text.to_string(), total_lines, false)
        } else {
            let start = offset.saturating_sub(1).min(total_lines);
            let end = (start + limit).min(total_lines);
            let selected: Vec<&str> = all_lines[start..end].to_vec();
            let lines_read = selected.len();
            (selected.join("\n"), lines_read, end < total_lines)
        };
        truncated = truncated || line_truncated;

        Ok(json!({
            "path": path_str,
            "content": selected,
            "lines_read": lines_read,
            "total_lines": total_lines,
            "truncated": truncated,
            "is_binary": false,
            "bytes_read": bytes_read
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn tool() -> FsReadTool {
        FsReadTool::new(SandboxValidator::new(
            zen_core::sandbox::SandboxMode::DangerFullAccess,
            vec![],
        ))
    }

    #[tokio::test]
    async fn byte_range_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("data.txt");
        std::fs::write(&path, "0123456789abcdefghij").unwrap();
        let res = tool()
            .invoke(json!({
                "path": path.to_str().unwrap(),
                "offset_bytes": 5,
                "length": 5
            }))
            .await
            .unwrap();
        assert_eq!(res["content"], "56789");
        assert_eq!(res["bytes_read"], 5);
        // More data remains beyond the requested range.
        assert_eq!(res["truncated"], true);
    }

    #[tokio::test]
    async fn truncation_on_large_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("big.txt");
        std::fs::write(&path, "x".repeat(2000)).unwrap();
        let res = tool()
            .invoke(json!({
                "path": path.to_str().unwrap(),
                "max_bytes": 1000
            }))
            .await
            .unwrap();
        assert_eq!(res["truncated"], true);
        assert_eq!(res["bytes_read"], 1000);
        assert_eq!(res["content"].as_str().unwrap().len(), 1000);
    }

    #[tokio::test]
    async fn binary_detection() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        std::fs::write(&path, [0u8, 1, 2, 3, 0, 255]).unwrap();
        let res = tool()
            .invoke(json!({ "path": path.to_str().unwrap() }))
            .await
            .unwrap();
        assert_eq!(res["is_binary"], true);
        assert_eq!(res["content"], "");
    }

    #[tokio::test]
    async fn base64_mode() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        let bytes = [0u8, 1, 2, 3, 0, 255];
        std::fs::write(&path, bytes).unwrap();
        let res = tool()
            .invoke(json!({
                "path": path.to_str().unwrap(),
                "encoding": "base64"
            }))
            .await
            .unwrap();
        assert_eq!(res["binary"], true);
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(res["content"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, bytes);
    }
}
