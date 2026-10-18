/// Caller-selected generation controls declared by the capability registry.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum PortableOption {
    Temperature,
    TopP,
    TopK,
    Seed,
    FrequencyPenalty,
    PresencePenalty,
    Stop,
    Logprobs,
    LogitBias,
    MinP,
    RepetitionPenalty,
    Prediction,
    Verbosity,
    Mirostat,
    ParallelToolCalls,
    Cache,
    PromptCacheTtl,
}

impl PortableOption {
    pub const PRESENCE_DRIVEN: [Self; 7] = [
        Self::Temperature,
        Self::TopP,
        Self::TopK,
        Self::Seed,
        Self::FrequencyPenalty,
        Self::PresencePenalty,
        Self::Stop,
    ];

    pub const ALL: [Self; 17] = [
        Self::Temperature,
        Self::TopP,
        Self::TopK,
        Self::Seed,
        Self::FrequencyPenalty,
        Self::PresencePenalty,
        Self::Stop,
        Self::Logprobs,
        Self::LogitBias,
        Self::MinP,
        Self::RepetitionPenalty,
        Self::Prediction,
        Self::Verbosity,
        Self::Mirostat,
        Self::ParallelToolCalls,
        Self::Cache,
        Self::PromptCacheTtl,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Temperature => "temperature",
            Self::TopP => "top_p",
            Self::TopK => "top_k",
            Self::Seed => "seed",
            Self::FrequencyPenalty => "frequency_penalty",
            Self::PresencePenalty => "presence_penalty",
            Self::Stop => "stop",
            Self::Logprobs => "logprobs",
            Self::LogitBias => "logit_bias",
            Self::MinP => "min_p",
            Self::RepetitionPenalty => "repetition_penalty",
            Self::Prediction => "prediction",
            Self::Verbosity => "verbosity",
            Self::Mirostat => "mirostat",
            Self::ParallelToolCalls => "parallel_tool_calls",
            Self::Cache => "cache",
            Self::PromptCacheTtl => "prompt_cache_ttl",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|option| option.name() == name)
    }
}
