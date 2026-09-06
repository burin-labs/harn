//! Cargo rustc wrapper for Harn's shared sccache setup.
//!
//! Cargo supplies `CARGO_BIN_EXE_*` only to compilation units that may embed a
//! package binary path. sccache 0.17's daemon-side compiler path can lose those
//! synthetic variables, so those rare units must stay in Cargo's process tree.
//! Every other compilation keeps using sccache, with the worktree-specific
//! target directory removed from its cache identity.

use std::env;
use std::ffi::{OsStr, OsString};
use std::process::{self, Command};

const CARGO_BIN_EXE_PREFIX: &str = "CARGO_BIN_EXE_";
const TRACE_ENV: &str = "HARN_SCCACHE_WRAPPER_TRACE";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Route {
    Direct,
    Sccache,
}

fn route_for_environment<I, K, V>(variables: I) -> Route
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
{
    if variables.into_iter().any(|(name, _)| {
        name.as_ref()
            .to_string_lossy()
            .starts_with(CARGO_BIN_EXE_PREFIX)
    }) {
        Route::Direct
    } else {
        Route::Sccache
    }
}

fn main() {
    let mut arguments = env::args_os();
    let _wrapper = arguments.next();
    let Some(rustc) = arguments.next() else {
        eprintln!("harn-sccache-wrapper: missing rustc executable");
        process::exit(2);
    };
    let rustc_arguments: Vec<OsString> = arguments.collect();
    let route = route_for_environment(env::vars_os());

    let mut command = match route {
        Route::Direct => Command::new(&rustc),
        Route::Sccache => {
            let mut command = Command::new("sccache");
            command.arg(&rustc).env_remove("CARGO_TARGET_DIR");
            command
        }
    };
    command.args(&rustc_arguments);

    if env::var_os(TRACE_ENV).is_some() {
        let label = match route {
            Route::Direct => "direct cargo-binary environment",
            Route::Sccache => "sccache",
        };
        eprintln!("harn-sccache-wrapper: route={label}");
    }

    match command.status() {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("harn-sccache-wrapper: failed to start compiler: {error}");
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{route_for_environment, Route};
    use std::ffi::OsString;

    #[test]
    fn cargo_binary_environment_routes_directly() {
        let variables = [
            (OsString::from("CARGO_PKG_NAME"), OsString::from("probe")),
            (
                OsString::from("CARGO_BIN_EXE_probe-bin"),
                OsString::from("placeholder:probe-bin"),
            ),
        ];

        assert_eq!(route_for_environment(variables), Route::Direct);
    }

    #[test]
    fn ordinary_compilation_routes_through_sccache() {
        let variables = [
            (OsString::from("CARGO_PKG_NAME"), OsString::from("probe")),
            (OsString::from("CARGO_TARGET_DIR"), OsString::from("target")),
        ];

        assert_eq!(route_for_environment(variables), Route::Sccache);
    }
}
