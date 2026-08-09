use harn_parser::{ShapeField, TypeExpr};
use std::collections::BTreeMap;

use crate::Constant;
use crate::{DataValue, Execution, GrantSet, PortableSourceModule};

use super::validation::{semantic_abi_fingerprint, validate_code};
use super::wire::{
    encode_wire_program, ArtifactReader, WireChunk, WireFunction, WireLocalSlot, WireParam,
    WireProgram,
};
use super::*;

const SOURCE: &str = "fn reduce(input) {\n  if input.reset { return {count: 0} }\n  return {count: input.count + 1}\n}";

const STATE_REDUCER_SOURCE: &str = r"
fn reduce(input) {
  let state = {count: 0, seen: []}
  for item in input.items {
    state.count = state.count + item
    state.seen = state.seen + [item]
  }
  for entry in input.weights {
    state.count = state.count + entry.value
  }
  return state
}
";

fn wrap_payload(payload: &[u8]) -> Vec<u8> {
    let digest = blake3::hash(payload);
    let mut bytes = Vec::with_capacity(HEADER_BYTES + payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&ARTIFACT_VERSION.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(digest.as_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn decoded_wire(artifact: &ProgramArtifact) -> WireProgram {
    ArtifactReader::new(&artifact.bytes()[HEADER_BYTES..], ArtifactLimits::default())
        .read_program()
        .unwrap()
}

fn artifact_from_wire(wire: &WireProgram) -> Vec<u8> {
    wrap_payload(&encode_wire_program(wire).unwrap())
}

#[test]
fn artifact_is_deterministic_and_round_trips() {
    let first = compile_program(SOURCE, "reduce", EntryKind::Function).unwrap();
    let second = compile_program(SOURCE, "reduce", EntryKind::Function).unwrap();
    assert_eq!(first.bytes(), second.bytes());
    let decoded = ProgramArtifact::decode(first.bytes(), ArtifactLimits::default()).unwrap();
    assert_eq!(decoded.digest(), first.digest());
    assert_eq!(decoded.entry(), "reduce");
}

#[test]
fn package_artifact_links_one_import_and_executes_its_function() {
    fn parse(source: &str) -> Vec<harn_parser::SNode> {
        let mut lexer = harn_lexer::Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = harn_parser::Parser::new(tokens);
        parser.parse().unwrap()
    }

    let root = parse("import { add } from \"lib\"\nfn reduce(input) { return add(input) }");
    let module = parse("pub fn add(input) { return input + 1 }");
    let artifact = compile_program_package(
        PortablePackageSource {
            root_program: root,
            root_imports: vec![PortableImport {
                path: "lib".to_string(),
                target: "lib".to_string(),
                selected_names: Some(vec!["add".to_string()]),
                namespace_alias: None,
                is_pub: false,
            }],
            modules: vec![PortableModuleSource {
                id: "lib".to_string(),
                program: module,
                imports: Vec::new(),
                exports: BTreeMap::from([("add".to_string(), PortableExportKind::Function)]),
                imported_enum_candidates: Vec::new(),
                source_file: Some("lib.harn".to_string()),
            }],
        },
        "reduce",
        EntryKind::Function,
    )
    .unwrap();
    assert_eq!(artifact.modules().len(), 1);
    let result = crate::start(&artifact, DataValue::Int(41), &GrantSet::pure());
    assert_eq!(
        result,
        Execution::Completed {
            value: DataValue::Int(42)
        }
    );
}

#[test]
fn source_package_manifest_uses_the_same_frontend_as_native_compilation() {
    let manifest = PortableSourcePackage {
        root_source: "import { add } from \"lib\"\nfn reduce(input) { return add(input) }"
            .to_string(),
        root_imports: vec![PortableImport {
            path: "lib".to_string(),
            target: "lib".to_string(),
            selected_names: Some(vec!["add".to_string()]),
            namespace_alias: None,
            is_pub: false,
        }],
        modules: vec![PortableSourceModule {
            id: "lib".to_string(),
            source: "pub fn add(input) { return input + 1 }".to_string(),
            imports: Vec::new(),
            exports: BTreeMap::from([("add".to_string(), PortableExportKind::Function)]),
            imported_enum_candidates: Vec::new(),
            source_file: Some("lib.harn".to_string()),
        }],
    };
    let artifact = compile_source_package(manifest, "reduce", EntryKind::Function).unwrap();
    assert_eq!(
        crate::start(&artifact, DataValue::Int(41), &GrantSet::pure()),
        Execution::Completed {
            value: DataValue::Int(42)
        }
    );
}

#[test]
fn namespace_member_calls_execute_the_exported_canonical_closure() {
    let artifact = compile_source_package(
        PortableSourcePackage {
            root_source:
                "import * as math from \"lib\"\nfn reduce(input) { return math.add(input) }"
                    .to_string(),
            root_imports: vec![PortableImport {
                path: "lib".to_string(),
                target: "lib".to_string(),
                selected_names: None,
                namespace_alias: Some("math".to_string()),
                is_pub: false,
            }],
            modules: vec![PortableSourceModule {
                id: "lib".to_string(),
                source: "pub fn add(input) { return input + 1 }".to_string(),
                imports: Vec::new(),
                exports: BTreeMap::from([("add".to_string(), PortableExportKind::Function)]),
                imported_enum_candidates: Vec::new(),
                source_file: Some("lib.harn".to_string()),
            }],
        },
        "reduce",
        EntryKind::Function,
    )
    .unwrap();

    assert_eq!(
        crate::start(&artifact, DataValue::Int(41), &GrantSet::pure()),
        Execution::Completed {
            value: DataValue::Int(42)
        }
    );
}

/// A namespace that escapes static analysis compiles to the whole-namespace
/// opcode rather than the narrowed one, so the projection must still carry
/// every export. The narrowed form is an optimization of known demand, not a
/// change to what a namespace means.
#[test]
fn escaped_namespace_projects_every_export() {
    let artifact = compile_source_package(
        PortableSourcePackage {
            root_source: "import * as math from \"lib\"\n\
                 fn reduce(input) { const ns = math; return ns.add(ns.double(input)) }"
                .to_string(),
            root_imports: vec![PortableImport {
                path: "lib".to_string(),
                target: "lib".to_string(),
                selected_names: None,
                namespace_alias: Some("math".to_string()),
                is_pub: false,
            }],
            modules: vec![PortableSourceModule {
                id: "lib".to_string(),
                source: "pub fn add(input) { return input + 1 }\n\
                     pub fn double(input) { return input * 2 }"
                    .to_string(),
                imports: Vec::new(),
                exports: BTreeMap::from([
                    ("add".to_string(), PortableExportKind::Function),
                    ("double".to_string(), PortableExportKind::Function),
                ]),
                imported_enum_candidates: Vec::new(),
                source_file: Some("lib.harn".to_string()),
            }],
        },
        "reduce",
        EntryKind::Function,
    )
    .unwrap();

    assert_eq!(
        crate::start(&artifact, DataValue::Int(20), &GrantSet::pure()),
        Execution::Completed {
            value: DataValue::Int(41)
        }
    );
}

#[test]
fn nested_package_calls_preserve_all_arguments_and_defaults() {
    let artifact = compile_source_package(
        PortableSourcePackage {
            root_source: r#"
                import * as ui from "renderer"
                fn reduce(input) {
                    return ui.app_resource("ui://portable", "Portable", input, {version: "2"})
                }
            "#
            .to_string(),
            root_imports: vec![PortableImport {
                path: "renderer".to_string(),
                target: "renderer".to_string(),
                selected_names: None,
                namespace_alias: Some("ui".to_string()),
                is_pub: false,
            }],
            modules: vec![
                PortableSourceModule {
                    id: "renderer".to_string(),
                    source: r#"
                        import { resource } from "resource"
                        pub fn app_resource(uri, name, tool_name, options = nil) {
                            return resource(uri, name, tool_name, options)
                        }
                    "#
                    .to_string(),
                    imports: vec![PortableImport {
                        path: "resource".to_string(),
                        target: "resource".to_string(),
                        selected_names: Some(vec!["resource".to_string()]),
                        namespace_alias: None,
                        is_pub: false,
                    }],
                    exports: BTreeMap::from([(
                        "app_resource".to_string(),
                        PortableExportKind::Function,
                    )]),
                    imported_enum_candidates: Vec::new(),
                    source_file: Some("renderer.harn".to_string()),
                },
                PortableSourceModule {
                    id: "resource".to_string(),
                    source: r"
                        pub fn resource(uri, name, tool_name, options = nil) {
                            return [uri, name, tool_name, options.version]
                        }
                    "
                    .to_string(),
                    imports: Vec::new(),
                    exports: BTreeMap::from([(
                        "resource".to_string(),
                        PortableExportKind::Function,
                    )]),
                    imported_enum_candidates: Vec::new(),
                    source_file: Some("resource.harn".to_string()),
                },
            ],
        },
        "reduce",
        EntryKind::Function,
    )
    .unwrap();

    assert_eq!(
        crate::start(
            &artifact,
            DataValue::String("logo.handle_event".to_string()),
            &GrantSet::pure(),
        ),
        Execution::Completed {
            value: DataValue::from_json(serde_json::json!([
                "ui://portable",
                "Portable",
                "logo.handle_event",
                "2",
            ]))
            .unwrap(),
        }
    );
}

#[test]
fn source_package_typechecks_imported_signatures_in_the_kernel() {
    let diagnostics = compile_source_package(
        PortableSourcePackage {
            root_source: "import { add } from \"lib\"\nfn reduce(input) { return add(\"wrong\") }"
                .to_string(),
            root_imports: vec![PortableImport {
                path: "lib".to_string(),
                target: "lib".to_string(),
                selected_names: Some(vec!["add".to_string()]),
                namespace_alias: None,
                is_pub: false,
            }],
            modules: vec![PortableSourceModule {
                id: "lib".to_string(),
                source: "pub fn add(input: int) -> int { return input + 1 }".to_string(),
                imports: Vec::new(),
                exports: BTreeMap::from([("add".to_string(), PortableExportKind::Function)]),
                imported_enum_candidates: Vec::new(),
                source_file: Some("lib.harn".to_string()),
            }],
        },
        "reduce",
        EntryKind::Function,
    )
    .unwrap_err();
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "HARN-TYP-006"
                && diagnostic.message.contains("expected int, found string")
        }),
        "imported callable signature was not checked: {diagnostics:?}"
    );
}

#[test]
fn reexported_callable_carries_its_owners_private_types() {
    let artifact = compile_source_package(
        PortableSourcePackage {
            root_source: "import { normalize } from \"facade\"\nfn reduce(input: string) -> string { return normalize({label: input}) }".to_string(),
            root_imports: vec![PortableImport {
                path: "facade".to_string(),
                target: "facade".to_string(),
                selected_names: Some(vec!["normalize".to_string()]),
                namespace_alias: None,
                is_pub: false,
            }],
            modules: vec![
                PortableSourceModule {
                    id: "facade".to_string(),
                    source: "pub import { normalize } from \"implementation\"".to_string(),
                    imports: vec![PortableImport {
                        path: "implementation".to_string(),
                        target: "implementation".to_string(),
                        selected_names: Some(vec!["normalize".to_string()]),
                        namespace_alias: None,
                        is_pub: true,
                    }],
                    exports: BTreeMap::from([(
                        "normalize".to_string(),
                        PortableExportKind::Function,
                    )]),
                    imported_enum_candidates: Vec::new(),
                    source_file: Some("facade.harn".to_string()),
                },
                PortableSourceModule {
                    id: "implementation".to_string(),
                    source: "type Options = {label: string}\npub fn normalize(options: Options) -> string { return options.label }".to_string(),
                    imports: Vec::new(),
                    exports: BTreeMap::from([(
                        "normalize".to_string(),
                        PortableExportKind::Function,
                    )]),
                    imported_enum_candidates: Vec::new(),
                    source_file: Some("implementation.harn".to_string()),
                },
            ],
        },
        "reduce",
        EntryKind::Function,
    )
    .unwrap();
    assert_eq!(
        crate::start(
            &artifact,
            DataValue::String("portable".into()),
            &GrantSet::pure()
        ),
        Execution::Completed {
            value: DataValue::String("portable".into())
        }
    );
}

#[test]
fn package_link_failures_are_structured_and_deterministic() {
    let missing_target = compile_source_package(
        PortableSourcePackage {
            root_source: "import { add } from \"lib\"\nfn reduce(input) { return add(input) }"
                .to_string(),
            root_imports: vec![PortableImport {
                path: "lib".to_string(),
                target: "missing".to_string(),
                selected_names: Some(vec!["add".to_string()]),
                namespace_alias: None,
                is_pub: false,
            }],
            modules: Vec::new(),
        },
        "reduce",
        EntryKind::Function,
    )
    .unwrap_err();
    assert_eq!(missing_target[0].code, "artifact_invalid_import");

    let imports_a = vec![PortableImport {
        path: "b".to_string(),
        target: "b".to_string(),
        selected_names: Some(vec!["b".to_string()]),
        namespace_alias: None,
        is_pub: false,
    }];
    let imports_b = vec![PortableImport {
        path: "a".to_string(),
        target: "a".to_string(),
        selected_names: Some(vec!["a".to_string()]),
        namespace_alias: None,
        is_pub: false,
    }];
    let cyclic = compile_source_package(
        PortableSourcePackage {
            root_source: "import { a } from \"a\"\nfn reduce(input) { return a(input) }"
                .to_string(),
            root_imports: vec![PortableImport {
                path: "a".to_string(),
                target: "a".to_string(),
                selected_names: Some(vec!["a".to_string()]),
                namespace_alias: None,
                is_pub: false,
            }],
            modules: vec![
                PortableSourceModule {
                    id: "a".to_string(),
                    source: "import { b } from \"b\"\npub fn a(input) { return input }".to_string(),
                    imports: imports_a,
                    exports: BTreeMap::from([("a".to_string(), PortableExportKind::Function)]),
                    imported_enum_candidates: Vec::new(),
                    source_file: Some("a.harn".to_string()),
                },
                PortableSourceModule {
                    id: "b".to_string(),
                    source: "import { a } from \"a\"\npub fn b(input) { return input }".to_string(),
                    imports: imports_b,
                    exports: BTreeMap::from([("b".to_string(), PortableExportKind::Function)]),
                    imported_enum_candidates: Vec::new(),
                    source_file: Some("b.harn".to_string()),
                },
            ],
        },
        "reduce",
        EntryKind::Function,
    )
    .unwrap();
    let Execution::Failed { diagnostic } =
        crate::start(&cyclic, DataValue::Int(1), &GrantSet::pure())
    else {
        panic!("cyclic package unexpectedly executed")
    };
    assert_eq!(diagnostic.code, "portable_import_cycle");
}

#[test]
fn portable_kernel_executes_state_reducer_with_list_and_map_iteration() {
    let artifact = compile_program(STATE_REDUCER_SOURCE, "reduce", EntryKind::Function).unwrap();
    let result = crate::start(
        &artifact,
        DataValue::Record(BTreeMap::from([
            (
                "items".to_string(),
                DataValue::List(vec![DataValue::Int(2), DataValue::Int(3)]),
            ),
            (
                "weights".to_string(),
                DataValue::Record(BTreeMap::from([
                    ("first".to_string(), DataValue::Int(5)),
                    ("second".to_string(), DataValue::Int(7)),
                ])),
            ),
        ])),
        &GrantSet::pure(),
    );
    assert_eq!(
        result,
        Execution::Completed {
            value: DataValue::Record(BTreeMap::from([
                ("count".to_string(), DataValue::Int(17)),
                (
                    "seen".to_string(),
                    DataValue::List(vec![DataValue::Int(2), DataValue::Int(3)]),
                ),
            ])),
        }
    );
}

#[test]
fn portable_compiler_policy_is_shared_by_every_adapter() {
    assert_eq!(
        "function".parse::<EntryKind>().unwrap(),
        EntryKind::Function
    );
    assert_eq!(
        "pipeline".parse::<EntryKind>().unwrap(),
        EntryKind::Pipeline
    );
    assert_eq!(
        "worker".parse::<EntryKind>().unwrap_err().code,
        "entry_kind"
    );

    let source = " ".repeat(crate::PORTABLE_MAX_SOURCE_BYTES + 1);
    let diagnostics = compile_program(&source, "main", EntryKind::Function).unwrap_err();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, "source_too_large");
}

#[test]
fn typed_defaults_compile_and_execute_through_the_shared_guard() {
    let artifact = compile_program(
        "fn with_default(value: int = 7) -> int { return value }\nfn reduce(input) -> int { return with_default() }",
        "reduce",
        EntryKind::Function,
    )
    .unwrap();
    assert_eq!(
        crate::start(&artifact, DataValue::Nil, &GrantSet::pure()),
        Execution::Completed {
            value: DataValue::Int(7)
        }
    );
}

#[test]
fn rejects_version_corruption_trailing_and_size() {
    let artifact = compile_program(SOURCE, "reduce", EntryKind::Function).unwrap();
    let mut version = artifact.bytes().to_vec();
    version[8..10].copy_from_slice(&ARTIFACT_VERSION.saturating_add(1).to_be_bytes());
    assert_eq!(
        ProgramArtifact::decode(&version, ArtifactLimits::default())
            .unwrap_err()
            .code,
        "artifact_version"
    );
    let mut corrupt = artifact.bytes().to_vec();
    *corrupt.last_mut().unwrap() ^= 1;
    assert_eq!(
        ProgramArtifact::decode(&corrupt, ArtifactLimits::default())
            .unwrap_err()
            .code,
        "artifact_corrupt"
    );
    let mut trailing = artifact.bytes().to_vec();
    trailing.push(0);
    assert_eq!(
        ProgramArtifact::decode(&trailing, ArtifactLimits::default())
            .unwrap_err()
            .code,
        "artifact_trailing_bytes"
    );
    let limits = ArtifactLimits {
        max_bytes: artifact.bytes().len() - 1,
        ..ArtifactLimits::default()
    };
    assert_eq!(
        ProgramArtifact::decode(artifact.bytes(), limits)
            .unwrap_err()
            .code,
        "artifact_too_large"
    );
}

#[test]
fn semantic_abi_is_checked_before_variable_length_fields() {
    let artifact = compile_program(SOURCE, "reduce", EntryKind::Function).unwrap();
    let mut payload = artifact.bytes()[HEADER_BYTES..].to_vec();
    payload[0] ^= 1;
    let bytes = wrap_payload(&payload);
    assert_eq!(
        ProgramArtifact::decode(&bytes, ArtifactLimits::default())
            .unwrap_err()
            .code,
        "artifact_semantic_abi"
    );

    let mut oversized_entry = semantic_abi_fingerprint().to_vec();
    oversized_entry.extend_from_slice(&65_u32.to_be_bytes());
    let bytes = wrap_payload(&oversized_entry);
    let limits = ArtifactLimits {
        max_string_bytes: 64,
        ..ArtifactLimits::default()
    };
    assert_eq!(
        ProgramArtifact::decode(&bytes, limits).unwrap_err().code,
        "artifact_allocation_limit"
    );
}

#[test]
fn semantic_abi_provenance_is_stable_hex() {
    let expected = semantic_abi_fingerprint()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(semantic_abi_fingerprint_hex(), expected);
    assert_eq!(expected.len(), 64);
}

#[test]
fn rejects_incoherent_callable_metadata() {
    let artifact = compile_program(
        "fn reduce(input, other) { return input + other }",
        "reduce",
        EntryKind::Function,
    )
    .unwrap();

    let mut wire = decoded_wire(&artifact);
    wire.functions[0].has_rest_param = true;
    wire.functions[0].params.clear();
    assert_eq!(
        ProgramArtifact::decode(&artifact_from_wire(&wire), ArtifactLimits::default())
            .unwrap_err()
            .code,
        "artifact_invalid_function"
    );

    let mut wire = decoded_wire(&artifact);
    wire.functions[0].is_stream = true;
    wire.functions[0].is_generator = false;
    assert_eq!(
        ProgramArtifact::decode(&artifact_from_wire(&wire), ArtifactLimits::default())
            .unwrap_err()
            .code,
        "artifact_invalid_function"
    );

    let mut wire = decoded_wire(&artifact);
    wire.functions[0].default_start = Some(0);
    assert_eq!(
        ProgramArtifact::decode(&artifact_from_wire(&wire), ArtifactLimits::default())
            .unwrap_err()
            .code,
        "artifact_invalid_function"
    );

    let mut wire = decoded_wire(&artifact);
    wire.functions[0].params[1].name = wire.functions[0].params[0].name.clone();
    assert_eq!(
        ProgramArtifact::decode(&artifact_from_wire(&wire), ArtifactLimits::default())
            .unwrap_err()
            .code,
        "artifact_invalid_function"
    );
}

#[test]
fn string_budget_covers_every_wire_metadata_family() {
    let entry = "entry";
    let constant = "constant";
    let source = "source";
    let local = "local";
    let function_name = entry;
    let type_param = "type";
    let nominal = "nominal";
    let parameter = "parameter";
    let applied = "Outer";
    let field = "field";
    let inner = "inner";
    let literal = "literal";
    let expected = [
        entry,
        constant,
        source,
        local,
        function_name,
        type_param,
        nominal,
        parameter,
        applied,
        field,
        inner,
        literal,
    ]
    .iter()
    .map(|value| value.len())
    .sum::<usize>();
    let wire = WireProgram {
        semantic_abi: semantic_abi_fingerprint(),
        entry: entry.into(),
        entry_kind: EntryKind::Function,
        expects_harness: false,
        chunks: vec![WireChunk {
            code: vec![crate::Op::Return as u8],
            constants: vec![Constant::String(constant.into())],
            lines: vec![1],
            columns: vec![1],
            source_file: Some(source.into()),
            functions: Vec::new(),
            binding_types: Vec::new(),
            local_slots: vec![WireLocalSlot {
                name: local.into(),
                mutable: false,
                scope_depth: 0,
            }],
            references_outer_names: false,
        }],
        functions: vec![WireFunction {
            name: function_name.into(),
            type_params: vec![type_param.into()],
            nominal_type_names: vec![nominal.into()],
            params: vec![WireParam {
                name: parameter.into(),
                type_expr: Some(TypeExpr::Union(vec![
                    TypeExpr::Applied {
                        name: applied.into(),
                        args: vec![TypeExpr::Shape(vec![ShapeField::synthetic(
                            field,
                            TypeExpr::Named(inner.into()),
                            false,
                        )])],
                    },
                    TypeExpr::LitString(literal.into()),
                ])),
                has_default: false,
            }],
            default_start: None,
            chunk: 0,
            is_generator: false,
            is_stream: false,
            has_rest_param: false,
            has_runtime_type_checks: true,
        }],
        root_imports: Vec::new(),
        modules: Vec::new(),
    };
    let limits = ArtifactLimits {
        max_string_bytes: expected - 1,
        ..ArtifactLimits::default()
    };
    assert_eq!(
        wire.validate_metadata(limits).unwrap_err().code,
        "artifact_strings_too_large"
    );
}

#[test]
fn typed_parameter_metadata_round_trips_into_the_hydrated_program() {
    let source = "fn reduce(input: { count: int }) -> int { return input.count }";
    let program = harn_parser::check_source_strict(source).unwrap();
    let compiled = Compiler::with_options(CompilerOptions::optimized())
        .compile_named_function_entry(&program, "reduce")
        .unwrap();
    let wire = WireProgram::from_image(
        &compiled.bootstrap,
        "reduce".into(),
        EntryKind::Function,
        compiled.expects_harness,
    )
    .unwrap();
    let before = wire
        .functions
        .iter()
        .flat_map(|function| &function.params)
        .find_map(|param| param.type_expr.clone())
        .expect("typed parameter is present in artifact metadata");
    let decoded = ArtifactReader::new(
        &encode_wire_program(&wire).unwrap(),
        ArtifactLimits::default(),
    )
    .read_program()
    .unwrap();
    let after = decoded
        .functions
        .iter()
        .flat_map(|function| &function.params)
        .find_map(|param| param.type_expr.clone())
        .expect("typed parameter survives artifact decoding");
    assert_eq!(before, after);
    let hydrated = decoded
        .validate_and_build(ArtifactLimits::default())
        .unwrap();
    let hydrated_type = hydrated.root.functions[0].params[0]
        .type_expr
        .clone()
        .expect("hydrated function retains parameter type metadata");
    assert_eq!(before, hydrated_type);
}

#[test]
fn rejects_jumps_to_operands_and_invalid_try_handlers() {
    let artifact = compile_program(SOURCE, "reduce", EntryKind::Function).unwrap();
    let mut wire = decoded_wire(&artifact);
    let (jump, code) = wire
        .chunks
        .iter_mut()
        .find_map(|chunk| {
            chunk
                .code
                .iter()
                .position(|byte| {
                    crate::Op::from_byte(*byte).is_some_and(|op| {
                        matches!(op, crate::Op::JumpIfFalse | crate::Op::JumpIfTrue)
                    })
                })
                .map(|jump| (jump, &mut chunk.code))
        })
        .expect("fixture contains a conditional jump");
    let operand_offset = jump + 1;
    code[operand_offset..operand_offset + 2]
        .copy_from_slice(&(operand_offset as u16).to_be_bytes());
    assert_eq!(
        ProgramArtifact::decode(&artifact_from_wire(&wire), ArtifactLimits::default())
            .unwrap_err()
            .code,
        "artifact_invalid_jump"
    );

    let try_code = [
        crate::Op::TryCatchSetup as u8,
        0,
        1,
        0,
        0,
        crate::Op::Return as u8,
    ];
    assert_eq!(
        validate_code(&try_code, &[Constant::String("Error".into())], 0, 0, 0, 0)
            .unwrap_err()
            .code,
        "artifact_invalid_jump"
    );
}

#[test]
fn validates_secondary_operands_and_builtin_identity() {
    let property = [crate::Op::SetLocalSlotProperty as u8, 0, 0, 0, 1];
    assert_eq!(
        validate_code(&property, &[Constant::String("field".into())], 0, 1, 0, 0)
            .unwrap_err()
            .code,
        "artifact_invalid_index"
    );

    let method = [crate::Op::MethodCall as u8, 0, 0, 0];
    assert_eq!(
        validate_code(&method, &[Constant::Int(0)], 0, 0, 0, 0)
            .unwrap_err()
            .code,
        "artifact_invalid_constant_type"
    );

    let check_type = [crate::Op::CheckType as u8, 0, 0, 0, 1];
    validate_code(
        &check_type,
        &[
            Constant::String("input".into()),
            Constant::String("int".into()),
        ],
        0,
        0,
        0,
        0,
    )
    .unwrap();

    // A narrowed namespace import is executable data now that the kernel
    // resolves package imports, so its operands must validate as three string
    // constants rather than being rejected as an unsupported opcode.
    let namespace_members = [crate::Op::NamespaceImportMembers as u8, 0, 0, 0, 1, 0, 2];
    validate_code(
        &namespace_members,
        &[
            Constant::String("./ui".into()),
            Constant::String("ui".into()),
            Constant::String("render".into()),
        ],
        0,
        0,
        0,
        0,
    )
    .unwrap();
    assert_eq!(
        validate_code(
            &namespace_members,
            &[
                Constant::String("./ui".into()),
                Constant::String("ui".into()),
                Constant::Int(0),
            ],
            0,
            0,
            0,
            0
        )
        .unwrap_err()
        .code,
        "artifact_invalid_constant_type"
    );

    let mut builtin = vec![crate::Op::CallBuiltin as u8];
    builtin.extend_from_slice(&0_u64.to_be_bytes());
    builtin.extend_from_slice(&0_u16.to_be_bytes());
    builtin.push(0);
    assert_eq!(
        validate_code(&builtin, &[Constant::String("len".into())], 0, 0, 0, 0)
            .unwrap_err()
            .code,
        "artifact_builtin_id_mismatch"
    );

    let name = "definitely_not_a_harn_builtin";
    let mut named_call = vec![crate::Op::CallBuiltin as u8];
    named_call.extend_from_slice(&crate::BuiltinId::from_name(name).raw().to_be_bytes());
    named_call.extend_from_slice(&0_u16.to_be_bytes());
    named_call.push(1);
    validate_code(&named_call, &[Constant::String(name.into())], 0, 0, 0, 0).unwrap();
}

#[test]
fn captured_callable_parameter_survives_named_call_validation() {
    let source = r"
fn wrap(callback) {
  return fn(value) { return callback(value) }
}
";
    let artifact = compile_program(source, "wrap", EntryKind::Function).unwrap();
    ProgramArtifact::decode(artifact.bytes(), ArtifactLimits::default()).unwrap();
}
