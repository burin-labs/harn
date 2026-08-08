use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CliLlmMockMode {
    #[default]
    Off,
    Replay {
        fixture_path: PathBuf,
    },
    Record {
        fixture_path: PathBuf,
    },
}

pub fn install_cli_llm_mock_mode(mode: &CliLlmMockMode) -> Result<(), String> {
    match mode {
        CliLlmMockMode::Off => {
            harn_vm::llm::clear_cli_llm_mock_mode();
            Ok(())
        }
        CliLlmMockMode::Replay { fixture_path } => {
            let text = std::fs::read_to_string(fixture_path)
                .map_err(|error| format!("failed to read {}: {error}", fixture_path.display()))?;
            let fixture = harn_vm::llm::parse_llm_mocks_jsonl(&text).map_err(|error| {
                format!(
                    "invalid LLM mock fixture in {}: {error}",
                    fixture_path.display()
                )
            })?;
            crate::skill_loader::emit_loader_warnings(&fixture.warnings);
            harn_vm::llm::clear_cli_llm_mock_mode();
            harn_vm::llm::install_cli_llm_mock_fixture(fixture);
            Ok(())
        }
        CliLlmMockMode::Record { .. } => {
            harn_vm::llm::clear_cli_llm_mock_mode();
            harn_vm::llm::enable_cli_llm_mock_recording();
            Ok(())
        }
    }
}

pub fn persist_cli_llm_mock_recording(mode: &CliLlmMockMode) -> Result<(), String> {
    let CliLlmMockMode::Record { fixture_path } = mode else {
        harn_vm::llm::clear_cli_llm_mock_mode();
        return Ok(());
    };
    let body = harn_vm::llm::serialize_llm_mock_fixture(harn_vm::llm::take_cli_llm_recordings())?;
    let result = harn_vm::atomic_io::atomic_write(fixture_path, body.as_bytes())
        .map_err(|error| format!("failed to write {}: {error}", fixture_path.display()));
    harn_vm::llm::clear_cli_llm_mock_mode();
    result
}
