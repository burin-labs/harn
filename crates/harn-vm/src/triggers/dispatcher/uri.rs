use crate::triggers::TriggerHandlerSpec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchUriError {
    Empty,
    MissingTarget { scheme: String },
    UnknownScheme(String),
}

impl std::fmt::Display for DispatchUriError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("handler URI cannot be empty"),
            Self::MissingTarget { scheme } => {
                write!(f, "{scheme} handler URI target cannot be empty")
            }
            Self::UnknownScheme(scheme) => write!(f, "unsupported handler URI scheme '{scheme}'"),
        }
    }
}

impl std::error::Error for DispatchUriError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchUri {
    Local {
        raw: String,
    },
    A2a {
        target: String,
        allow_cleartext: bool,
    },
    Worker {
        queue: String,
    },
}

impl DispatchUri {
    pub fn parse(raw: &str) -> Result<Self, DispatchUriError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(DispatchUriError::Empty);
        }
        if let Some(target) = raw.strip_prefix("a2a://") {
            if target.is_empty() {
                return Err(DispatchUriError::MissingTarget {
                    scheme: "a2a".to_string(),
                });
            }
            return Ok(Self::A2a {
                target: target.to_string(),
                allow_cleartext: false,
            });
        }
        if let Some(queue) = raw.strip_prefix("worker://") {
            if queue.is_empty() {
                return Err(DispatchUriError::MissingTarget {
                    scheme: "worker".to_string(),
                });
            }
            return Ok(Self::Worker {
                queue: queue.to_string(),
            });
        }
        if let Some((scheme, _)) = raw.split_once("://") {
            return Err(DispatchUriError::UnknownScheme(scheme.to_string()));
        }
        Ok(Self::Local {
            raw: raw.to_string(),
        })
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Local { .. } => "local",
            Self::A2a { .. } => "a2a",
            Self::Worker { .. } => "worker",
        }
    }

    pub fn target_uri(&self) -> String {
        match self {
            Self::Local { raw } => raw.clone(),
            Self::A2a { target, .. } => format!("a2a://{target}"),
            Self::Worker { queue } => format!("worker://{queue}"),
        }
    }
}

impl From<&TriggerHandlerSpec> for DispatchUri {
    fn from(value: &TriggerHandlerSpec) -> Self {
        match value {
            TriggerHandlerSpec::Local { raw, .. } => Self::Local { raw: raw.clone() },
            TriggerHandlerSpec::A2a {
                target,
                allow_cleartext,
            } => Self::A2a {
                target: target.clone(),
                allow_cleartext: *allow_cleartext,
            },
            TriggerHandlerSpec::Worker { queue } => Self::Worker {
                queue: queue.clone(),
            },
        }
    }
}
