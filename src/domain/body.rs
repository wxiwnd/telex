use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

/// Body block: base64 payload kept out of the envelope structure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Body {
    pub encoding: Encoding,
    pub media_type: MediaType,
    pub data: String,
    pub bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    Base64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaType {
    #[serde(rename = "text/plain; charset=utf-8")]
    TextPlain,
    #[serde(rename = "text/markdown; charset=utf-8")]
    TextMarkdown,
}

impl MediaType {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaType::TextPlain => "text/plain; charset=utf-8",
            MediaType::TextMarkdown => "text/markdown; charset=utf-8",
        }
    }
}

impl Body {
    pub fn from_text(text: &str) -> Self {
        Self::from_text_with(text, MediaType::TextPlain)
    }

    pub fn from_text_with(text: &str, media_type: MediaType) -> Self {
        let bytes = text.as_bytes();
        Self {
            encoding: Encoding::Base64,
            media_type,
            data: STANDARD.encode(bytes),
            bytes: bytes.len(),
        }
    }

    pub fn decode(&self) -> Result<Vec<u8>, base64::DecodeError> {
        STANDARD.decode(&self.data)
    }

    pub fn decode_text(&self) -> Result<String, String> {
        let raw = self.decode().map_err(|e| e.to_string())?;
        String::from_utf8(raw).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_text() {
        let body = Body::from_text("hello, world");
        assert_eq!(body.bytes, "hello, world".len());
        assert_eq!(body.decode_text().unwrap(), "hello, world");
    }

    #[test]
    fn roundtrips_non_ascii_text() {
        // Unicode escapes keep the source ASCII while exercising UTF-8 bodies.
        let body = Body::from_text("\u{4f60}\u{597d}");
        assert_eq!(body.bytes, 6);
        assert_eq!(body.decode_text().unwrap(), "\u{4f60}\u{597d}");
    }

    #[test]
    fn media_type_strings() {
        assert_eq!(MediaType::TextPlain.as_str(), "text/plain; charset=utf-8");
        assert_eq!(
            serde_json::to_string(&MediaType::TextMarkdown).unwrap(),
            "\"text/markdown; charset=utf-8\""
        );
    }
}
