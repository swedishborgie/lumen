//! Video codec identifier shared across encoder backends and the WebRTC layer.

use serde::{Deserialize, Serialize};

/// Supported video codecs for encoding and WebRTC negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    #[default]
    H264,
    H265,
    Vp9,
    Av1,
}

impl std::fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::H264 => write!(f, "h264"),
            Self::H265 => write!(f, "h265"),
            Self::Vp9 => write!(f, "vp9"),
            Self::Av1 => write!(f, "av1"),
        }
    }
}

impl std::str::FromStr for VideoCodec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "h264" => Ok(Self::H264),
            "h265" | "hevc" => Ok(Self::H265),
            "vp9" => Ok(Self::Vp9),
            "av1" => Ok(Self::Av1),
            other => Err(format!("unknown codec {other:?}; expected h264, h265, vp9, or av1")),
        }
    }
}
