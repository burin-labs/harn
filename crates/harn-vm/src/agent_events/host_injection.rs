use serde::{Deserialize, Serialize};

use crate::security::{DetectorVerdict, TrustLevel};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectionDelivery {
    TurnBoundary,
    #[default]
    Immediate,
    AfterNextToolCall,
}

impl InjectionDelivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TurnBoundary => "turn_boundary",
            Self::Immediate => "immediate",
            Self::AfterNextToolCall => "after_next_tool_call",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "turn_boundary" => Some(Self::TurnBoundary),
            "immediate" => Some(Self::Immediate),
            "after_next_tool_call" => Some(Self::AfterNextToolCall),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInjectionProvenance {
    pub initiator: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub ts_ms: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SanitizationAction {
    Passed,
    Summarized,
    Pointerized,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SanitizationVerdict {
    pub trust: TrustLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detector: Option<DetectorVerdict>,
    pub action: SanitizationAction,
    pub original_bytes: u64,
    pub delivered_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentFlavor {
    Image,
    TextFrame,
    FrameRing,
    File,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRendering {
    ImageBlock,
    DescriptionPlusPointer,
    PointerOnly,
    InlineText,
}
