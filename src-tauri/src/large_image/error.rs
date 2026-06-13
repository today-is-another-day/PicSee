use serde::Serialize;

/// 统一错误类型，供前端按 code 做 i18n 映射。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LargeImageError {
    pub code: &'static str,
    pub message: String,
}

impl LargeImageError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn session_not_found(session_id: u64) -> Self {
        Self::new(
            "SESSION_NOT_FOUND",
            format!("Session {session_id} not found"),
        )
    }

    pub fn tile_out_of_range(x: u32, y: u32) -> Self {
        Self::new(
            "TILE_OUT_OF_RANGE",
            format!("Tile ({x}, {y}) is out of image range"),
        )
    }

    pub fn tiles_unavailable() -> Self {
        Self::new(
            "TILES_UNAVAILABLE",
            "This image format only supports preview rendering",
        )
    }

    pub fn unsupported_format(msg: impl Into<String>) -> Self {
        Self::new("UNSUPPORTED_FORMAT", msg)
    }

    pub fn io(msg: impl Into<String>) -> Self {
        Self::new("IO_ERROR", msg)
    }

    pub fn decode(msg: impl Into<String>) -> Self {
        Self::new("DECODE_ERROR", msg)
    }

    pub fn system_decode(msg: impl Into<String>) -> Self {
        Self::new("SYSTEM_DECODE_ERROR", msg)
    }

    pub fn encode(msg: impl Into<String>) -> Self {
        Self::new("ENCODE_ERROR", msg)
    }
}
