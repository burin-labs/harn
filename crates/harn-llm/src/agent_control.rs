//! Delivery timing carried by host messages into an agent loop.

use serde::{Deserialize, Serialize};

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
