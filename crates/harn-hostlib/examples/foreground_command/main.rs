//! Minimal command-capable embedding with process lifetime dispatch.

use harn_hostlib::{BuiltinRegistry, HostlibCapability};
use harn_vm::value::{DictMap, VmDictExt};
use harn_vm::VmValue;
use std::io::{self, Write};

#[cfg(test)]
mod tests;

fn main() {
    #[cfg(test)]
    if std::env::var_os("HOSTLIB_TEST_WITHOUT_GUARDIAN").is_some() {
        return finish(command(std::env::args().skip(2).collect()));
    }
    if harn_hostlib::process::owner_death::run_if_requested() {
        return;
    }
    #[cfg(test)]
    if std::env::args().nth(1).as_deref() != Some("--client") {
        return tests::run();
    }
    #[cfg(test)]
    let argv = std::env::args().skip(2).collect();
    #[cfg(not(test))]
    let argv = std::env::args().skip(1).collect();
    finish(command(argv));
}

fn finish(result: Result<i32, Box<dyn std::error::Error>>) {
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

fn command(argv: Vec<String>) -> Result<i32, Box<dyn std::error::Error>> {
    let mut registry = BuiltinRegistry::new();
    harn_hostlib::tools::ToolsCapability.register_builtins(&mut registry);
    let mut request = DictMap::new();
    request.put(
        "argv",
        VmValue::list(argv.into_iter().map(VmValue::string).collect()),
    );
    request.put("env_mode", VmValue::string("replace"));
    let builtin = registry
        .find("hostlib_tools_run_command")
        .expect("command builtin");
    let value = (builtin.handler)(&[VmValue::dict(request)])?;
    let VmValue::Dict(response) = value else {
        return Err("command response is not a dict".into());
    };
    let Some(VmValue::String(stdout)) = response.get("stdout") else {
        return Err("command stdout is missing".into());
    };
    let Some(VmValue::String(stderr)) = response.get("stderr") else {
        return Err("command stderr is missing".into());
    };
    let Some(VmValue::Int(code)) = response.get("exit_code") else {
        return Err("command exit code is missing".into());
    };
    io::stdout().write_all(stdout.as_bytes())?;
    io::stderr().write_all(stderr.as_bytes())?;
    Ok((*code).try_into()?)
}
