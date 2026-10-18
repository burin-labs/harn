//! Host capability expectations shared with the VM and hostlib suites.

use harn_vm as vm;

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../harn-vm/tests/support/sandbox_host.rs"
));
