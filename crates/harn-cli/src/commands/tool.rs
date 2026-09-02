mod registry;
mod scaffold;

pub(crate) use registry::{print_registry_completions, print_registry_schema, run_registry};
pub(crate) use scaffold::run_new;
