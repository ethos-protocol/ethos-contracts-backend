/// Request decompression middleware for Ethos-Protocol backend.
///
/// # Overview
///
/// Clients may compress request bodies using `Content-Encoding: gzip` to reduce
/// bandwidth. This module provides:
///
/// - A [`DecompressionConfig`] that is loaded from environment variables.
/// - A [`decompress_request`] axum extractor / layer helper that decodes gzip
///   bodies before the inner handler sees them.
/// - Decompression size limits to guard against zip-bomb payloads.
///
/// # Usage
///
/// ```rust,ignore
/// use ethos_protocol_backend::decompression::{DecompressionConfig, decompress_body};
///
/// // In build_router, add the tower-http RequestDecompressionLayer:
/// use tower_http::decompression::RequestDecompressionLayer;
/// let app = Router::new()
///     /* routes … */
///     .layer(RequestDecompressionLayer::new());
/// ```
///
/// The tower-http `RequestDecompressionLayer` transparently decodes
/// `Content-Encoding: gzip`, `deflate`, and `br` bodies before they reach
/// handlers. The config below controls the max allowed decompressed body size;
/// oversized bodies yield **413 Payload Too Large**, and malformed compressed
/// bodies yield **400 Bad Request** (see [`DecompressionError`]).
use std::io::Read;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Decompression configuration loaded from environment variables.
///
/// | Variable | Default | Description |
/// |---|---|---|
/// | `DECOMP_MAX_BODY_BYTES` | `10_485_760` (10 MiB) | Maximum decompressed body size |
/// | `DECOMP_ENABLED` | `true` | Toggle middleware on/off |
#[derive(Debug, Clone)]
pub struct DecompressionConfig {
    /// Maximum number of bytes allowed after decompression.
    /// Requests whose decompressed body exceeds this limit are rejected with 413.
    pub max_body_bytes: usize,
    /// Whether request decompression is enabled at all.
    pub enabled: bool,
}

impl Default for DecompressionConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: 10 * 1024 * 1024, // 10 MiB
            enabled: true,
        }
    }
}

impl DecompressionConfig {
    /// Build config from environment variables, falling back to defaults.
    pub fn from_env() -> Self {
        Self {
            max_body_bytes: std::env::var("DECOMP_MAX_BODY_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10 * 1024 * 1024),
            enabled: std::env::var("DECOMP_ENABLED")
                .ok()
                .map(|v| v.to_lowercase() != "false" && v != "0")
                .unwrap_or(true),
        }
    }
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// Why a request body could not be decompressed.
///
/// Request bodies are attacker-controlled, so every failure is reported as a
/// value instead of panicking. Use [`DecompressionError::status_code`] (or the
/// [`IntoResponse`] impl) to turn an error into the right HTTP response.
#[derive(Debug, thiserror::Error)]
pub enum DecompressionError {
    /// The body is not valid data for the declared `Content-Encoding`
    /// (truncated stream, bad header, corrupt checksum, ...).
    #[error("malformed {encoding} body: {source}")]
    Malformed {
        encoding: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// The decompressed body is larger than the configured limit.
    #[error("decompressed body exceeds limit of {limit} bytes")]
    TooLarge { limit: usize },
}

impl DecompressionError {
    /// HTTP status to return: **400** for malformed bodies, **413** for
    /// bodies that exceed the size limit.
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::Malformed { .. } => StatusCode::BAD_REQUEST,
            Self::TooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        }
    }
}

impl IntoResponse for DecompressionError {
    fn into_response(self) -> Response {
        (self.status_code(), self.to_string()).into_response()
    }
}

// ── Decompression helpers ──────────────────────────────────────────────────────

/// Supported `Content-Encoding` values that we can decompress.
#[derive(Debug, PartialEq, Eq)]
pub enum ContentEncoding {
    Gzip,
    Deflate,
    Identity,
    Unknown(String),
}

impl ContentEncoding {
    /// Parse from the raw `Content-Encoding` header value.
    pub fn from_header(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "gzip" | "x-gzip" => Self::Gzip,
            "deflate" => Self::Deflate,
            "identity" | "" => Self::Identity,
            other => Self::Unknown(other.to_string()),
        }
    }
}

/// Read all of `decoder` into memory, stopping one byte past `max_bytes`.
///
/// Reading with a hard cap avoids unbounded memory growth on
/// decompression-bomb payloads.
fn read_limited<R: Read>(
    decoder: R,
    max_bytes: usize,
    encoding: &'static str,
) -> Result<Vec<u8>, DecompressionError> {
    let mut out = Vec::new();
    let cap = u64::try_from(max_bytes).map_or(u64::MAX, |m| m.saturating_add(1));
    decoder
        .take(cap)
        .read_to_end(&mut out)
        .map_err(|source| DecompressionError::Malformed { encoding, source })?;

    if out.len() > max_bytes {
        return Err(DecompressionError::TooLarge { limit: max_bytes });
    }

    Ok(out)
}

/// Decompress `bytes` that are gzip-encoded.
///
/// The `max_bytes` guard prevents decompression-bomb attacks: if the
/// decompressed stream exceeds `max_bytes` the function returns
/// [`DecompressionError::TooLarge`]. Corrupt or truncated input returns
/// [`DecompressionError::Malformed`].
pub fn decompress_gzip(bytes: &[u8], max_bytes: usize) -> Result<Vec<u8>, DecompressionError> {
    read_limited(flate2::read::GzDecoder::new(bytes), max_bytes, "gzip")
}

/// Decompress `bytes` that are deflate-encoded (raw zlib).
///
/// Same error semantics as [`decompress_gzip`].
pub fn decompress_deflate(bytes: &[u8], max_bytes: usize) -> Result<Vec<u8>, DecompressionError> {
    read_limited(flate2::read::ZlibDecoder::new(bytes), max_bytes, "deflate")
}

/// Decompress `body_bytes` according to `content_encoding`.
///
/// Returns `Ok(decompressed)` or a [`DecompressionError`] that maps to
/// 400 (malformed) or 413 (too large).
/// `Identity` (no encoding) passes the bytes through unchanged.
/// Unknown encodings are also passed through unchanged (letting the inner
/// handler deal with them — most will 400 on their own).
pub fn decompress_body(
    body_bytes: &[u8],
    content_encoding: &str,
    config: &DecompressionConfig,
) -> Result<Vec<u8>, DecompressionError> {
    if !config.enabled {
        return Ok(body_bytes.to_vec());
    }

    match ContentEncoding::from_header(content_encoding) {
        ContentEncoding::Gzip => decompress_gzip(body_bytes, config.max_body_bytes),
        ContentEncoding::Deflate => decompress_deflate(body_bytes, config.max_body_bytes),
        ContentEncoding::Identity | ContentEncoding::Unknown(_) => {
            // Pass through; identity / unknown encodings are not our concern.
            Ok(body_bytes.to_vec())
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn gzip_encode(data: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data)?;
        encoder.finish()
    }

    fn deflate_encode(data: &[u8]) -> std::io::Result<Vec<u8>> {
        use flate2::write::ZlibEncoder;
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data)?;
        encoder.finish()
    }

    #[test]
    fn test_content_encoding_from_header() {
        assert_eq!(ContentEncoding::from_header("gzip"), ContentEncoding::Gzip);
        assert_eq!(
            ContentEncoding::from_header("x-gzip"),
            ContentEncoding::Gzip
        );
        assert_eq!(
            ContentEncoding::from_header("deflate"),
            ContentEncoding::Deflate
        );
        assert_eq!(
            ContentEncoding::from_header("identity"),
            ContentEncoding::Identity
        );
        assert_eq!(ContentEncoding::from_header(""), ContentEncoding::Identity);
        assert_eq!(
            ContentEncoding::from_header("br"),
            ContentEncoding::Unknown("br".to_string())
        );
    }

    #[test]
    fn test_decompress_gzip_roundtrip() -> TestResult {
        let original = b"hello ethos-protocol vault data";
        let compressed = gzip_encode(original)?;
        let decompressed = decompress_gzip(&compressed, 1024)?;
        assert_eq!(decompressed, original);
        Ok(())
    }

    #[test]
    fn test_decompress_deflate_roundtrip() -> TestResult {
        let original = b"hello ethos-protocol deflate data";
        let compressed = deflate_encode(original)?;
        let decompressed = decompress_deflate(&compressed, 1024)?;
        assert_eq!(decompressed, original);
        Ok(())
    }

    #[test]
    fn test_decompress_gzip_exceeds_limit() -> TestResult {
        // Compress a 100-byte payload but allow only 50 bytes out.
        let original = vec![b'x'; 100];
        let compressed = gzip_encode(&original)?;
        let err = decompress_gzip(&compressed, 50).err().ok_or("expected an error")?;
        assert!(matches!(err, DecompressionError::TooLarge { limit: 50 }));
        assert!(err.to_string().contains("exceeds limit"));
        assert_eq!(err.status_code(), StatusCode::PAYLOAD_TOO_LARGE);
        Ok(())
    }

    #[test]
    fn test_decompress_malformed_gzip_is_bad_request() {
        let config = DecompressionConfig::default();
        let result = decompress_body(b"definitely not gzip", "gzip", &config);
        let Err(err) = result else {
            panic!("malformed gzip body must not decompress");
        };
        assert!(matches!(err, DecompressionError::Malformed { encoding: "gzip", .. }));
        assert_eq!(err.status_code(), StatusCode::BAD_REQUEST);
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_decompress_truncated_gzip_is_bad_request() -> TestResult {
        let compressed = gzip_encode(b"a body that will be cut short")?;
        let truncated = &compressed[..compressed.len() / 2];
        let err = decompress_gzip(truncated, 1024)
            .err()
            .ok_or("truncated gzip body must not decompress")?;
        assert_eq!(err.status_code(), StatusCode::BAD_REQUEST);
        Ok(())
    }

    #[test]
    fn test_decompress_malformed_deflate_is_bad_request() -> TestResult {
        let config = DecompressionConfig::default();
        let err = decompress_body(b"\x00\x01\x02 not zlib", "deflate", &config)
            .err()
            .ok_or("malformed deflate body must not decompress")?;
        assert!(matches!(err, DecompressionError::Malformed { encoding: "deflate", .. }));
        assert_eq!(err.status_code(), StatusCode::BAD_REQUEST);
        Ok(())
    }

    #[test]
    fn test_decompress_body_gzip() -> TestResult {
        let config = DecompressionConfig::default();
        let original = b"vault payload compressed";
        let compressed = gzip_encode(original)?;
        let out = decompress_body(&compressed, "gzip", &config)?;
        assert_eq!(out, original);
        Ok(())
    }

    #[test]
    fn test_decompress_body_identity_passthrough() -> TestResult {
        let config = DecompressionConfig::default();
        let data = b"plain body no encoding";
        let out = decompress_body(data, "identity", &config)?;
        assert_eq!(out, data);
        Ok(())
    }

    #[test]
    fn test_decompress_body_disabled() -> TestResult {
        let config = DecompressionConfig {
            enabled: false,
            ..Default::default()
        };
        // Even gzip bytes should pass through unchanged when disabled.
        let original = b"raw bytes";
        let out = decompress_body(original, "gzip", &config)?;
        assert_eq!(out, original);
        Ok(())
    }

    #[test]
    fn test_config_defaults() {
        let cfg = DecompressionConfig::default();
        assert_eq!(cfg.max_body_bytes, 10 * 1024 * 1024);
        assert!(cfg.enabled);
    }

    #[test]
    fn test_config_from_env() {
        std::env::set_var("DECOMP_MAX_BODY_BYTES", "2048");
        std::env::set_var("DECOMP_ENABLED", "true");
        let cfg = DecompressionConfig::from_env();
        assert_eq!(cfg.max_body_bytes, 2048);
        assert!(cfg.enabled);
        std::env::remove_var("DECOMP_MAX_BODY_BYTES");
        std::env::remove_var("DECOMP_ENABLED");
    }

    #[test]
    fn test_config_disabled_via_env() {
        std::env::set_var("DECOMP_ENABLED", "false");
        let cfg = DecompressionConfig::from_env();
        assert!(!cfg.enabled);
        std::env::remove_var("DECOMP_ENABLED");
    }
}
