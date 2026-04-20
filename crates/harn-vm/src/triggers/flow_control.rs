use std::rc::Rc;
use std::time::Duration;

use crate::value::VmClosure;

#[derive(Clone)]
pub struct TriggerExpressionSpec {
    pub raw: String,
    pub closure: Rc<VmClosure>,
}

impl std::fmt::Debug for TriggerExpressionSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TriggerExpressionSpec")
            .field("raw", &self.raw)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct TriggerConcurrencyConfig {
    pub key: Option<TriggerExpressionSpec>,
    pub max: u32,
}

#[derive(Clone, Debug)]
pub struct TriggerThrottleConfig {
    pub key: Option<TriggerExpressionSpec>,
    pub period: Duration,
    pub max: u32,
}

#[derive(Clone, Debug)]
pub struct TriggerRateLimitConfig {
    pub key: Option<TriggerExpressionSpec>,
    pub period: Duration,
    pub max: u32,
}

#[derive(Clone, Debug)]
pub struct TriggerDebounceConfig {
    pub key: TriggerExpressionSpec,
    pub period: Duration,
}

#[derive(Clone, Debug)]
pub struct TriggerSingletonConfig {
    pub key: Option<TriggerExpressionSpec>,
}

#[derive(Clone, Debug)]
pub struct TriggerBatchConfig {
    pub key: Option<TriggerExpressionSpec>,
    pub size: u32,
    pub timeout: Duration,
}

#[derive(Clone, Debug)]
pub struct TriggerPriorityOrderConfig {
    pub key: TriggerExpressionSpec,
    pub order: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct TriggerFlowControlConfig {
    pub concurrency: Option<TriggerConcurrencyConfig>,
    pub throttle: Option<TriggerThrottleConfig>,
    pub rate_limit: Option<TriggerRateLimitConfig>,
    pub debounce: Option<TriggerDebounceConfig>,
    pub singleton: Option<TriggerSingletonConfig>,
    pub batch: Option<TriggerBatchConfig>,
    pub priority: Option<TriggerPriorityOrderConfig>,
}

pub fn parse_flow_control_duration(raw: &str) -> Result<Duration, String> {
    let trimmed = raw.trim();
    if trimmed.len() < 2 {
        return Err(format!("invalid duration '{raw}': expected <int><unit>"));
    }
    let split = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .ok_or_else(|| format!("invalid duration '{raw}': missing unit suffix"))?;
    if split == 0 || split == trimmed.len() {
        return Err(format!("invalid duration '{raw}': expected <int><unit>"));
    }
    let value = trimmed[..split]
        .parse::<u64>()
        .map_err(|_| format!("invalid duration '{raw}': expected integer prefix"))?;
    if value == 0 {
        return Err(format!(
            "invalid duration '{raw}': duration must be positive"
        ));
    }
    let factor = match trimmed[split..].to_ascii_lowercase().as_str() {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 60 * 60 * 24,
        "w" => 60 * 60 * 24 * 7,
        other => {
            return Err(format!(
                "invalid duration '{raw}': unsupported unit '{other}', expected s/m/h/d/w"
            ))
        }
    };
    Ok(Duration::from_secs(value.saturating_mul(factor)))
}
