use harn_parser::{ShapeField, TypeExpr};

use crate::{Constant, OperandKind};

use super::{ArtifactLimits, Diagnostic, SEMANTIC_ABI_DOMAIN};

pub(super) struct MetadataBudget {
    limits: ArtifactLimits,
    instruction_bytes: usize,
    constants: usize,
    string_bytes: usize,
    metadata_entries: usize,
    type_nodes: usize,
}

impl MetadataBudget {
    pub(super) fn new(limits: ArtifactLimits) -> Self {
        Self {
            limits,
            instruction_bytes: 0,
            constants: 0,
            string_bytes: 0,
            metadata_entries: 0,
            type_nodes: 0,
        }
    }

    fn add(
        value: &mut usize,
        amount: usize,
        limit: usize,
        code: &str,
        message: &str,
    ) -> Result<(), Diagnostic> {
        *value = value
            .checked_add(amount)
            .ok_or_else(|| Diagnostic::artifact(code, message))?;
        if *value > limit {
            return Err(Diagnostic::artifact(code, message));
        }
        Ok(())
    }

    pub(super) fn instructions(&mut self, amount: usize) -> Result<(), Diagnostic> {
        Self::add(
            &mut self.instruction_bytes,
            amount,
            self.limits.max_instructions,
            "artifact_too_many_instructions",
            "artifact instruction bytes exceed limit",
        )
    }

    pub(super) fn constants(&mut self, amount: usize) -> Result<(), Diagnostic> {
        Self::add(
            &mut self.constants,
            amount,
            self.limits.max_constants,
            "artifact_too_many_constants",
            "artifact constants exceed limit",
        )
    }

    pub(super) fn string(&mut self, value: &str) -> Result<(), Diagnostic> {
        Self::add(
            &mut self.string_bytes,
            value.len(),
            self.limits.max_string_bytes,
            "artifact_strings_too_large",
            "artifact string bytes exceed limit",
        )
    }

    pub(super) fn metadata(&mut self, amount: usize) -> Result<(), Diagnostic> {
        Self::add(
            &mut self.metadata_entries,
            amount,
            self.limits.max_metadata_entries,
            "artifact_metadata_too_large",
            "artifact metadata entry count exceeds limit",
        )
    }

    pub(super) fn type_expr(
        &mut self,
        type_expr: &TypeExpr,
        depth: usize,
    ) -> Result<(), Diagnostic> {
        self.type_node(depth)?;
        match type_expr {
            TypeExpr::Named(name) | TypeExpr::LitString(name) => self.string(name),
            TypeExpr::Union(items) | TypeExpr::Intersection(items) | TypeExpr::Tuple(items) => {
                self.metadata(items.len())?;
                for item in items {
                    self.type_expr(item, depth + 1)?;
                }
                Ok(())
            }
            TypeExpr::Shape(fields) => self.shape_fields(fields, depth),
            TypeExpr::OpenShape { fields, rests } => {
                self.shape_fields(fields, depth)?;
                self.metadata(rests.len())?;
                for rest in rests {
                    self.type_expr(rest, depth + 1)?;
                }
                Ok(())
            }
            TypeExpr::List(inner)
            | TypeExpr::Iter(inner)
            | TypeExpr::Generator(inner)
            | TypeExpr::Stream(inner)
            | TypeExpr::Owned(inner) => self.type_expr(inner, depth + 1),
            TypeExpr::DictType(key, value) => {
                self.type_expr(key, depth + 1)?;
                self.type_expr(value, depth + 1)
            }
            TypeExpr::Applied { name, args } => {
                self.string(name)?;
                self.metadata(args.len())?;
                for arg in args {
                    self.type_expr(arg, depth + 1)?;
                }
                Ok(())
            }
            TypeExpr::FnType {
                params,
                return_type,
            } => {
                self.metadata(params.len())?;
                for param in params {
                    self.type_expr(param, depth + 1)?;
                }
                self.type_expr(return_type, depth + 1)
            }
            TypeExpr::Never | TypeExpr::LitInt(_) => Ok(()),
        }
    }

    pub(super) fn type_node(&mut self, depth: usize) -> Result<(), Diagnostic> {
        if depth > self.limits.max_type_depth {
            return Err(Diagnostic::artifact(
                "artifact_type_too_deep",
                "artifact parameter type nesting exceeds limit",
            ));
        }
        Self::add(
            &mut self.type_nodes,
            1,
            self.limits.max_type_nodes,
            "artifact_too_many_type_nodes",
            "artifact parameter type node count exceeds limit",
        )
    }

    fn shape_fields(&mut self, fields: &[ShapeField], depth: usize) -> Result<(), Diagnostic> {
        self.metadata(fields.len())?;
        for field in fields {
            self.string(&field.name)?;
            self.type_expr(&field.type_expr, depth + 1)?;
        }
        Ok(())
    }
}

pub(super) fn semantic_abi_fingerprint() -> [u8; 32] {
    let mut hash = AbiHasher::new();
    hash.bytes(SEMANTIC_ABI_DOMAIN);
    hash.bytes(&crate::opcode_abi_fingerprint());
    hash.len(crate::portable_builtin::PortableBuiltin::ALL.len());
    for builtin in crate::portable_builtin::PortableBuiltin::ALL {
        hash.string(builtin.name());
    }
    let manifest = harn_capability_contracts::manifest();
    hash.len(manifest.len());
    for entry in manifest {
        hash.string(entry.name);
        hash.string(entry.canonical_name);
        hash_signature(&mut hash, entry.signature);
        hash_contract(&mut hash, &entry.contract);
    }
    hash.finish()
}

struct AbiHasher(blake3::Hasher);

impl AbiHasher {
    fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    fn byte(&mut self, value: u8) {
        self.0.update(&[value]);
    }

    fn u16(&mut self, value: u16) {
        self.0.update(&value.to_be_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.0.update(&value.to_be_bytes());
    }

    fn len(&mut self, value: usize) {
        self.0.update(&(value as u64).to_be_bytes());
    }

    fn bytes(&mut self, value: &[u8]) {
        self.len(value.len());
        self.0.update(value);
    }

    fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn finish(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}

fn hash_signature(hash: &mut AbiHasher, signature: &harn_builtin_meta::BuiltinSignature) {
    hash.string(signature.name);
    hash.len(signature.params.len());
    for param in signature.params {
        hash.string(param.name);
        hash_ty(hash, param.ty);
        hash.byte(param.optional.into());
    }
    hash_ty(hash, signature.returns);
    hash.len(signature.type_params.len());
    for name in signature.type_params {
        hash.string(name);
    }
    hash.byte(signature.has_rest.into());
    hash.len(signature.where_clauses.len());
    for (parameter, interface) in signature.where_clauses {
        hash.string(parameter);
        hash.string(interface);
    }
}

fn hash_ty(hash: &mut AbiHasher, ty: harn_builtin_meta::Ty) {
    use harn_builtin_meta::Ty;
    match ty {
        Ty::Named(name) => {
            hash.byte(0);
            hash.string(name);
        }
        Ty::Generic(name) => {
            hash.byte(1);
            hash.string(name);
        }
        Ty::Any => hash.byte(2),
        Ty::Optional(inner) => {
            hash.byte(3);
            hash_ty(hash, *inner);
        }
        Ty::Apply(name, args) => {
            hash.byte(4);
            hash.string(name);
            hash.len(args.len());
            for arg in args {
                hash_ty(hash, *arg);
            }
        }
        Ty::Union(items) => {
            hash.byte(5);
            hash.len(items.len());
            for item in items {
                hash_ty(hash, *item);
            }
        }
        Ty::Fn(params, result) => {
            hash.byte(6);
            hash.len(params.len());
            for param in params {
                hash_ty(hash, *param);
            }
            hash_ty(hash, *result);
        }
        Ty::Shape(fields) => {
            hash.byte(7);
            hash.len(fields.len());
            for field in fields {
                hash.string(field.name);
                hash_ty(hash, field.ty);
                hash.byte(field.optional.into());
            }
        }
        Ty::OpenShape(fields, rests) => {
            hash.byte(12);
            hash.len(fields.len());
            for field in fields {
                hash.string(field.name);
                hash_ty(hash, field.ty);
                hash.byte(field.optional.into());
            }
            hash.len(rests.len());
            for rest in rests {
                hash_ty(hash, *rest);
            }
        }
        Ty::SchemaOf(name) => {
            hash.byte(8);
            hash.string(name);
        }
        Ty::Never => hash.byte(9),
        Ty::LitInt(value) => {
            hash.byte(10);
            hash.i64(value);
        }
        Ty::LitString(value) => {
            hash.byte(11);
            hash.string(value);
        }
    }
}

fn hash_contract(hash: &mut AbiHasher, contract: &harn_builtin_meta::BuiltinContract) {
    use harn_builtin_meta::BuiltinExposure;
    match contract.exposure {
        BuiltinExposure::Undeclared => hash.byte(0),
        BuiltinExposure::PureGlobal => hash.byte(1),
        BuiltinExposure::CapabilityFunction { authority_argument } => {
            hash.byte(2);
            hash.u16(authority_argument);
        }
        BuiltinExposure::HarnessMethod { capability, method } => {
            hash.byte(3);
            hash.string(capability.field_name());
            hash.string(method);
        }
        BuiltinExposure::PrivilegedWire => hash.byte(4),
        BuiltinExposure::RuntimeInternal => hash.byte(5),
    }
    hash.len(contract.effects.len());
    for effect in contract.effects {
        hash_effect(hash, effect);
    }
}

fn hash_effect(hash: &mut AbiHasher, effect: &harn_builtin_meta::EffectSpec) {
    use harn_builtin_meta::{EffectAccess, EffectKind, ResourceSelector};
    hash.byte(match effect.kind {
        EffectKind::Stdio => 0,
        EffectKind::Fs => 1,
        EffectKind::Env => 2,
        EffectKind::Clock => 3,
        EffectKind::Random => 4,
        EffectKind::Network => 5,
        EffectKind::Process => 6,
        EffectKind::Llm => 7,
        EffectKind::Tool => 8,
        EffectKind::Mcp => 9,
        EffectKind::Host => 10,
        EffectKind::Worker => 11,
        EffectKind::Secret => 12,
        EffectKind::Observability => 13,
        EffectKind::Channel => 14,
        EffectKind::State => 15,
    });
    hash.byte(match effect.access {
        EffectAccess::Read => 0,
        EffectAccess::Write => 1,
        EffectAccess::Mutate => 2,
        EffectAccess::Observe => 3,
    });
    hash.len(effect.resources.len());
    for resource in effect.resources {
        match resource {
            ResourceSelector::Argument(index) => {
                hash.byte(0);
                hash.u16(*index);
            }
            ResourceSelector::Field { argument, path } => {
                hash.byte(1);
                hash.u16(*argument);
                hash.len(path.len());
                for component in *path {
                    hash.string(component);
                }
            }
            ResourceSelector::EachArgument(index) => {
                hash.byte(2);
                hash.u16(*index);
            }
            ResourceSelector::Constant(value) => {
                hash.byte(3);
                hash.string(value);
            }
            ResourceSelector::Dynamic => hash.byte(4),
        }
    }
}

pub(super) fn validate_code(
    code: &[u8],
    constants: &[Constant],
    functions: usize,
    locals: usize,
    binding_types: usize,
    chunk: usize,
) -> Result<(), Diagnostic> {
    let mut instruction_boundaries = vec![false; code.len()];
    let mut jump_targets = Vec::new();
    let mut ip = 0usize;
    while ip < code.len() {
        instruction_boundaries[ip] = true;
        let op = crate::Op::from_byte(code[ip]).ok_or_else(|| {
            Diagnostic::artifact(
                "artifact_invalid_opcode",
                format!(
                    "chunk {chunk} has invalid opcode 0x{:02x} at {ip}",
                    code[ip]
                ),
            )
        })?;
        let width = op.instruction_len();
        if ip.checked_add(width).is_none_or(|end| end > code.len()) {
            return Err(Diagnostic::artifact(
                "artifact_truncated_instruction",
                format!("chunk {chunk} instruction at {ip} is truncated"),
            ));
        }
        let mut operand_offset = ip + 1;
        let mut builtin_id = None;
        let mut builtin_name = None;
        for operand in op.operands() {
            match operand {
                OperandKind::ImmediateU8 => {}
                OperandKind::ImmediateU16 => {}
                OperandKind::BuiltinIdU64 => {
                    builtin_id = Some(u64::from_be_bytes(
                        code[operand_offset..operand_offset + 8]
                            .try_into()
                            .expect("instruction width checked"),
                    ));
                }
                OperandKind::ConstantU16 | OperandKind::StringConstantU16 => {
                    let index = read_code_u16(code, operand_offset);
                    let Some(constant) = constants.get(index) else {
                        return Err(Diagnostic::artifact(
                            "artifact_invalid_index",
                            format!(
                                "chunk {chunk} {} at {ip} references missing constant {index}",
                                op.name()
                            ),
                        ));
                    };
                    if matches!(operand, OperandKind::StringConstantU16) {
                        let Constant::String(name) = constant else {
                            return Err(Diagnostic::artifact(
                                "artifact_invalid_constant_type",
                                format!(
                                    "chunk {chunk} {} at {ip} requires string constant {index}",
                                    op.name()
                                ),
                            ));
                        };
                        builtin_name = Some(name.as_str());
                    }
                }
                OperandKind::LocalU16 => {
                    let index = read_code_u16(code, operand_offset);
                    if index >= locals {
                        return Err(Diagnostic::artifact(
                            "artifact_invalid_index",
                            format!(
                                "chunk {chunk} {} at {ip} references missing local slot {index}",
                                op.name()
                            ),
                        ));
                    }
                }
                OperandKind::BindingTypeU16 => {
                    let index = read_code_u16(code, operand_offset);
                    if index >= binding_types {
                        return Err(Diagnostic::artifact(
                            "artifact_invalid_index",
                            format!(
                                "chunk {chunk} {} at {ip} references missing binding type {index}",
                                op.name()
                            ),
                        ));
                    }
                }
                OperandKind::FunctionU16 => {
                    let index = read_code_u16(code, operand_offset);
                    if index >= functions {
                        return Err(Diagnostic::artifact(
                            "artifact_invalid_index",
                            format!(
                                "chunk {chunk} {} at {ip} references missing function {index}",
                                op.name()
                            ),
                        ));
                    }
                }
                OperandKind::JumpU16 => {
                    jump_targets.push((ip, op, read_code_u16(code, operand_offset)));
                }
            }
            operand_offset += operand.width();
        }
        if let (Some(id), Some(name)) = (builtin_id, builtin_name) {
            // `CallBuiltin` is the historical bytecode name for every named
            // call. Its callee may be a function, imported binding, or captured
            // closure parameter, so the decoder cannot classify it from a
            // builtin table. The name-derived ID still detects corruption;
            // execution resolves Harn bindings before its closed builtin table.
            if crate::BuiltinId::from_name(name).raw() != id {
                return Err(Diagnostic::artifact(
                    "artifact_builtin_id_mismatch",
                    format!(
                        "chunk {chunk} {} at {ip} has a builtin ID that does not match `{name}`",
                        op.name()
                    ),
                ));
            }
        }
        ip += width;
    }
    for (source, op, target) in jump_targets {
        if target >= code.len() || !instruction_boundaries[target] {
            return Err(Diagnostic::artifact(
                "artifact_invalid_jump",
                format!(
                    "chunk {chunk} {} at {source} targets non-instruction offset {target}",
                    op.name()
                ),
            ));
        }
    }
    Ok(())
}

fn read_code_u16(code: &[u8], offset: usize) -> usize {
    u16::from_be_bytes([code[offset], code[offset + 1]]) as usize
}
