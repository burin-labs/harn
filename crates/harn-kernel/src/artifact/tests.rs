use harn_parser::{ShapeField, TypeExpr};

use crate::Constant;

use super::validation::{semantic_abi_fingerprint, validate_code};
use super::wire::{
    encode_wire_program, ArtifactReader, WireChunk, WireFunction, WireLocalSlot, WireParam,
    WireProgram,
};
use super::*;

const SOURCE: &str = "fn reduce(input) {\n  if input.reset { return {count: 0} }\n  return {count: input.count + 1}\n}";

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

    let source = " ".repeat(PORTABLE_SOURCE_MAX_BYTES + 1);
    let diagnostics = compile_program(&source, "main", EntryKind::Function).unwrap_err();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, "source_too_large");
}

#[test]
fn typed_defaults_fail_at_the_portable_compile_boundary() {
    let diagnostics = compile_program(
        "fn reduce(input: int = 1) -> int { return input }",
        "reduce",
        EntryKind::Function,
    )
    .unwrap_err();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, "unsupported_portable_typed_default");
    assert_eq!(diagnostics[0].line, Some(1));
    assert_eq!(diagnostics[0].column, Some(11));
}

#[test]
fn rejects_version_corruption_trailing_and_size() {
    let artifact = compile_program(SOURCE, "reduce", EntryKind::Function).unwrap();
    let mut version = artifact.bytes().to_vec();
    version[8..10].copy_from_slice(&(ARTIFACT_VERSION + 1).to_be_bytes());
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
    let hydrated_type = hydrated.functions[0].params[0]
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
        validate_code(
            &try_code,
            &[Constant::String("Error".into())],
            &Default::default(),
            0,
            0,
            0,
        )
        .unwrap_err()
        .code,
        "artifact_invalid_jump"
    );
}

#[test]
fn validates_secondary_operands_and_builtin_identity() {
    let property = [crate::Op::SetLocalSlotProperty as u8, 0, 0, 0, 1];
    assert_eq!(
        validate_code(
            &property,
            &[Constant::String("field".into())],
            &Default::default(),
            0,
            1,
            0,
        )
        .unwrap_err()
        .code,
        "artifact_invalid_index"
    );

    let method = [crate::Op::MethodCall as u8, 0, 0, 0];
    assert_eq!(
        validate_code(&method, &[Constant::Int(0)], &Default::default(), 0, 0, 0)
            .unwrap_err()
            .code,
        "artifact_invalid_constant_type"
    );

    let check_type = [crate::Op::CheckType as u8, 0, 0, 0, 1];
    assert_eq!(
        validate_code(
            &check_type,
            &[
                Constant::String("input".into()),
                Constant::String("int".into()),
            ],
            &Default::default(),
            0,
            0,
            0,
        )
        .unwrap_err()
        .code,
        "artifact_unsupported_opcode"
    );

    let namespace_members = [crate::Op::NamespaceImportMembers as u8, 0, 0, 0, 1, 0, 2];
    assert_eq!(
        validate_code(
            &namespace_members,
            &[
                Constant::String("./ui".into()),
                Constant::String("ui".into()),
                Constant::String("render".into()),
            ],
            &Default::default(),
            0,
            0,
            0,
        )
        .unwrap_err()
        .code,
        "artifact_unsupported_opcode"
    );

    let mut builtin = vec![crate::Op::CallBuiltin as u8];
    builtin.extend_from_slice(&0_u64.to_be_bytes());
    builtin.extend_from_slice(&0_u16.to_be_bytes());
    builtin.push(0);
    assert_eq!(
        validate_code(
            &builtin,
            &[Constant::String("len".into())],
            &Default::default(),
            0,
            0,
            0,
        )
        .unwrap_err()
        .code,
        "artifact_builtin_id_mismatch"
    );

    let name = "json_parse";
    let mut unsupported = vec![crate::Op::CallBuiltin as u8];
    unsupported.extend_from_slice(&crate::BuiltinId::from_name(name).raw().to_be_bytes());
    unsupported.extend_from_slice(&0_u16.to_be_bytes());
    unsupported.push(1);
    assert_eq!(
        validate_code(
            &unsupported,
            &[Constant::String(name.into())],
            &Default::default(),
            0,
            0,
            0,
        )
        .unwrap_err()
        .code,
        "artifact_unsupported_builtin"
    );
}
