//! Gemini history and tool-choice encoding over neutral request values.

mod generate_content;
mod interactions;

pub use generate_content::{
    encode_history as generate_content_history, tool_config as generate_content_tool_config,
    GenerateContentHistory,
};
pub use interactions::{
    encode_history as interactions_history, tool_choice_config as interactions_tool_choice,
    InteractionsHistory, STEP_FUNCTION_CALL, STEP_FUNCTION_RESULT, STEP_MODEL_OUTPUT, STEP_THOUGHT,
    STEP_USER_INPUT,
};

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

#[cfg(test)]
mod tests;
