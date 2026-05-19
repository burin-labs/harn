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
    Persona {
        name: String,
    },
    AutoResume {
        worker_id: String,
    },
    /// Dispatcher composes triggers with named agent pools (#1889). The
    /// closure handed to the pool is built per-event by the binding's
    /// `task_factory`; the descriptor here carries only routing metadata so
    /// the URI stays serializable and comparable.
    SpawnToPool {
        pool: String,
        priority_from: Option<String>,
        key_from: Option<String>,
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
        if let Some(name) = raw.strip_prefix("persona://") {
            if name.is_empty() {
                return Err(DispatchUriError::MissingTarget {
                    scheme: "persona".to_string(),
                });
            }
            return Ok(Self::Persona {
                name: name.to_string(),
            });
        }
        if let Some(pool) = raw.strip_prefix("pool://") {
            if pool.is_empty() {
                return Err(DispatchUriError::MissingTarget {
                    scheme: "pool".to_string(),
                });
            }
            return Ok(Self::SpawnToPool {
                pool: pool.to_string(),
                priority_from: None,
                key_from: None,
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
            Self::Persona { .. } => "persona",
            Self::AutoResume { .. } => "auto_resume",
            Self::SpawnToPool { .. } => "spawn_to_pool",
        }
    }

    pub fn target_uri(&self) -> String {
        match self {
            Self::Local { raw } => raw.clone(),
            Self::A2a { target, .. } => format!("a2a://{target}"),
            Self::Worker { queue } => format!("worker://{queue}"),
            Self::Persona { name } => format!("persona://{name}"),
            Self::AutoResume { worker_id } => format!("auto_resume://{worker_id}"),
            Self::SpawnToPool { pool, .. } => format!("pool://{pool}"),
        }
    }

    pub fn trust_boundary(&self) -> &'static str {
        match self {
            Self::Local { .. } => "local_process",
            Self::A2a { .. } => "federated_a2a",
            Self::Worker { .. } => "event_log_worker_queue",
            Self::Persona { .. } => "persona_runtime",
            Self::AutoResume { .. } => "local_process",
            Self::SpawnToPool { .. } => "local_process",
        }
    }

    pub fn execution_location(&self) -> &'static str {
        match self {
            Self::Local { .. } => "in_process",
            Self::A2a { .. } => "remote",
            Self::Worker { .. } => "queued",
            Self::Persona { .. } => "managed_persona",
            Self::AutoResume { .. } => "in_process",
            Self::SpawnToPool { .. } => "pool_queued",
        }
    }

    pub fn remote_identity(&self) -> Option<String> {
        match self {
            Self::A2a { target, .. } => Some(crate::a2a::target_agent_label(target)),
            _ => None,
        }
    }

    pub fn dispatch_boundary_metadata(
        &self,
    ) -> std::collections::BTreeMap<String, serde_json::Value> {
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert(
            "trust_boundary".to_string(),
            serde_json::json!(self.trust_boundary()),
        );
        metadata.insert(
            "execution_location".to_string(),
            serde_json::json!(self.execution_location()),
        );
        if let Some(remote_identity) = self.remote_identity() {
            metadata.insert(
                "remote_identity".to_string(),
                serde_json::json!(remote_identity),
            );
        }
        metadata
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
            TriggerHandlerSpec::Persona { binding } => Self::Persona {
                name: binding.name.clone(),
            },
            TriggerHandlerSpec::AutoResume { worker_id } => Self::AutoResume {
                worker_id: worker_id.clone(),
            },
            TriggerHandlerSpec::SpawnToPool {
                pool,
                priority_from,
                key_from,
                ..
            } => Self::SpawnToPool {
                pool: pool.clone(),
                priority_from: priority_from.clone(),
                key_from: key_from.clone(),
            },
        }
    }
}
