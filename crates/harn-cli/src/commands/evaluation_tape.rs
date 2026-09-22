//! One CLI projection of the VM-owned evaluation scope.
use harn_vm::llm::decision::replay::EvaluationReplayScope;
use std::path::PathBuf;

#[derive(Clone, Debug, Default)]
pub struct EvaluationReplayOptions {
    pub tape: Option<PathBuf>,
    pub cache: bool,
}

pub struct EvaluationReplaySession {
    scope: EvaluationReplayScope,
    output: Option<PathBuf>,
}

impl EvaluationReplayOptions {
    pub fn install(&self, vm: &mut harn_vm::Vm) -> Result<Option<EvaluationReplaySession>, String> {
        let (scope, output) = if let Some(path) = &self.tape {
            if self.cache {
                return Err("evaluation tape and cache are mutually exclusive".into());
            }
            if path.exists() {
                let tape = harn_vm::testbench::tape::EventTape::load(path)?;
                (
                    EvaluationReplayScope::replay(&tape).map_err(|e| e.to_string())?,
                    None,
                )
            } else {
                (EvaluationReplayScope::record(), Some(path.clone()))
            }
        } else if self.cache {
            (EvaluationReplayScope::cache(), None)
        } else {
            return Ok(None);
        };
        vm.set_evaluation_replay(scope.clone());
        Ok(Some(EvaluationReplaySession { scope, output }))
    }
}

impl EvaluationReplaySession {
    pub fn finish(self, succeeded: bool) -> Result<(), String> {
        let tape = self.scope.finish().map_err(|e| e.to_string())?;
        if succeeded {
            if let (Some(tape), Some(path)) = (tape, self.output) {
                tape.persist_new(&path)?;
            }
        }
        Ok(())
    }
}
