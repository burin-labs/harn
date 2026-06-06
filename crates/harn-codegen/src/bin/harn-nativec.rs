//! `harn-nativec` — experimental Harn-to-native compiler CLI.
//!
//! Compiles a single scalar Harn function to machine code (JIT or object file)
//! and, optionally, runs it. This binary is the human-facing front door to the
//! `harn-codegen` crate and is never part of the distributed `harn` binary.
//!
//! ```text
//! harn-nativec <file.harn> <function> [--object <out.o>] [--run <arg>...] [--debug]
//! ```

use std::process::ExitCode;

use harn_codegen::{
    analyze_named, emit_object, evaluate, jit_compile, ScalarFunction, ScalarType, ScalarValue,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("harn-nativec: {message}");
            ExitCode::FAILURE
        }
    }
}

struct Options {
    file: String,
    function: String,
    object_out: Option<String>,
    run_args: Option<Vec<String>>,
    debug: bool,
}

fn run(args: &[String]) -> Result<(), String> {
    let opts = parse_args(args)?;

    let source = std::fs::read_to_string(&opts.file)
        .map_err(|e| format!("cannot read `{}`: {e}", opts.file))?;

    let scalar = analyze_named(&source, &opts.function).map_err(|e| e.to_string())?;

    println!(
        "fn {} : ({}) -> {}",
        scalar.name,
        scalar
            .params
            .iter()
            .map(|ty| ty.harn_name())
            .collect::<Vec<_>>()
            .join(", "),
        scalar.ret
    );
    println!(
        "  scalar subset OK — {} block(s), {} local slot(s)",
        scalar.blocks.len(),
        scalar.slot_count()
    );
    if opts.debug {
        println!("{scalar:#?}");
    }

    if let Some(path) = &opts.object_out {
        let artifact = emit_object(&scalar).map_err(|e| e.to_string())?;
        std::fs::write(path, &artifact.bytes).map_err(|e| format!("cannot write `{path}`: {e}"))?;
        println!(
            "  wrote {} byte object exporting `{}` to {path}",
            artifact.bytes.len(),
            artifact.symbol
        );
    }

    if let Some(raw_args) = &opts.run_args {
        let parsed = parse_run_args(&scalar, raw_args)?;
        let native = jit_compile(&scalar).map_err(|e| e.to_string())?;
        let jit_result = native.call(&parsed).map_err(|e| e.to_string())?;
        // Cross-check against the reference interpreter — they must agree.
        let ref_result = evaluate(&scalar, &parsed).map_err(|e| e.to_string())?;
        if jit_result != ref_result {
            return Err(format!(
                "internal mismatch: jit={jit_result} reference={ref_result}"
            ));
        }
        println!("  => {jit_result}");
    }

    Ok(())
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut positional = Vec::new();
    let mut object_out = None;
    let mut run_args = None;
    let mut debug = false;

    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--object" => {
                idx += 1;
                object_out = Some(args.get(idx).ok_or("--object requires a path")?.clone());
            }
            "--run" => {
                // Everything after --run is a positional argument value.
                run_args = Some(args[idx + 1..].to_vec());
                break;
            }
            "--debug" => debug = true,
            "--help" | "-h" => return Err(usage()),
            other if other.starts_with("--") => {
                return Err(format!("unknown flag `{other}`\n{}", usage()));
            }
            other => positional.push(other.to_string()),
        }
        idx += 1;
    }

    if positional.len() != 2 {
        return Err(usage());
    }
    Ok(Options {
        file: positional[0].clone(),
        function: positional[1].clone(),
        object_out,
        run_args,
        debug,
    })
}

fn parse_run_args(scalar: &ScalarFunction, raw: &[String]) -> Result<Vec<ScalarValue>, String> {
    if raw.len() != scalar.params.len() {
        return Err(format!(
            "`{}` takes {} argument(s), got {}",
            scalar.name,
            scalar.params.len(),
            raw.len()
        ));
    }
    raw.iter()
        .zip(&scalar.params)
        .map(|(text, ty)| parse_scalar(text, *ty))
        .collect()
}

fn parse_scalar(text: &str, ty: ScalarType) -> Result<ScalarValue, String> {
    match ty {
        ScalarType::Int => text
            .parse::<i64>()
            .map(ScalarValue::Int)
            .map_err(|_| format!("`{text}` is not an int")),
        ScalarType::Float => text
            .parse::<f64>()
            .map(ScalarValue::Float)
            .map_err(|_| format!("`{text}` is not a float")),
        ScalarType::Bool => text
            .parse::<bool>()
            .map(ScalarValue::Bool)
            .map_err(|_| format!("`{text}` is not a bool")),
    }
}

fn usage() -> String {
    "usage: harn-nativec <file.harn> <function> [--object <out.o>] [--run <arg>...] [--debug]"
        .to_string()
}
