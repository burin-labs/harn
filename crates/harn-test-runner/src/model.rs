use std::path::PathBuf;
use std::sync::Arc;

use harn_parser::SNode;
use harn_vm::{IsolateValue, VmValue};

use crate::fixtures::TestFixture;

/// A single executable test discovered from one source file.
///
/// This is an engine-internal carrier exposed only so host adapters can
/// compile and execute cases without owning discovery policy.
#[doc(hidden)]
#[derive(Clone)]
pub struct TestCase {
    pub file: PathBuf,
    pub name: String,
    pub pipeline_name: String,
    pub source: Arc<String>,
    pub program: Arc<Vec<SNode>>,
    pub imported_enum_candidates: Arc<Vec<String>>,
    pub serial_group: Option<String>,
    pub weight: usize,
    pub args: Vec<VmValue>,
    pub fixture: Option<TestFixture>,
    pub file_fixture_value: Option<IsolateValue>,
    pub compiled_entry: Option<Arc<harn_vm::CompiledCallableEntry>>,
    pub compiled_file_fixture_entry:
        Option<Result<Arc<harn_vm::CompiledCallableEntry>, harn_vm::CompileError>>,
    pub trusted_host_dispatch: bool,
}
