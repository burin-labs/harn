use harn_parser::{ShapeField, TypeExpr};

use crate::Constant;

use super::{WireChunk, WireFunction, WireLocalSlot, WireParam, WireProgram};
use crate::artifact::validation::{semantic_abi_fingerprint, MetadataBudget};
use crate::artifact::{ArtifactLimits, Diagnostic, EntryKind};

pub(in crate::artifact) struct ArtifactReader<'a> {
    bytes: &'a [u8],
    offset: usize,
    limits: ArtifactLimits,
    budget: MetadataBudget,
}

impl<'a> ArtifactReader<'a> {
    pub(in crate::artifact) fn new(bytes: &'a [u8], limits: ArtifactLimits) -> Self {
        Self {
            bytes,
            offset: 0,
            limits,
            budget: MetadataBudget::new(limits),
        }
    }

    pub(in crate::artifact) fn read_program(mut self) -> Result<WireProgram, Diagnostic> {
        let semantic_abi: [u8; 32] = self
            .take(32, "semantic ABI fingerprint")?
            .try_into()
            .expect("fixed-size fingerprint");
        if semantic_abi != semantic_abi_fingerprint() {
            return Err(Diagnostic::artifact(
                "artifact_semantic_abi",
                "artifact compiler/opcode/capability contract does not match this kernel",
            ));
        }
        let entry = self.string("entry name")?;
        let entry_kind = match self.u8("entry kind")? {
            0 => EntryKind::Function,
            1 => EntryKind::Pipeline,
            value => {
                return Err(Diagnostic::artifact(
                    "artifact_malformed",
                    format!("artifact has invalid entry-kind tag {value}"),
                ))
            }
        };
        let expects_harness = self.boolean("expects-harness flag")?;
        let chunk_count = self.count("chunks", self.limits.max_chunks)?;
        if chunk_count == 0 {
            return Err(Diagnostic::artifact(
                "artifact_malformed",
                "artifact has no root chunk",
            ));
        }
        let function_count = self.count("functions", self.limits.max_functions)?;

        let mut chunks = Vec::with_capacity(chunk_count);
        for chunk_id in 0..chunk_count {
            chunks.push(self.chunk(chunk_id, function_count)?);
        }
        let mut functions = Vec::with_capacity(function_count);
        for function_id in 0..function_count {
            functions.push(self.function(function_id)?);
        }
        if self.offset != self.bytes.len() {
            return Err(Diagnostic::artifact(
                "artifact_trailing_payload",
                format!(
                    "artifact payload has {} unread bytes",
                    self.bytes.len() - self.offset
                ),
            ));
        }
        Ok(WireProgram {
            semantic_abi,
            entry,
            entry_kind,
            expects_harness,
            chunks,
            functions,
        })
    }

    fn chunk(&mut self, chunk_id: usize, function_count: usize) -> Result<WireChunk, Diagnostic> {
        let code_len = self.count("instruction bytes", self.limits.max_instructions)?;
        self.budget.instructions(code_len)?;
        let code = self.take(code_len, "instruction bytes")?.to_vec();

        let constant_count = self.count("constants", self.limits.max_constants)?;
        self.budget.constants(constant_count)?;
        let mut constants = Vec::with_capacity(constant_count);
        for _ in 0..constant_count {
            constants.push(self.constant()?);
        }
        let lines = self.u32_vec_exact(code_len, "source lines")?;
        let columns = self.u32_vec_exact(code_len, "source columns")?;
        let source_file = self.optional_string("source file")?;

        let function_refs = self.count("function references", self.limits.max_metadata_entries)?;
        self.budget.metadata(function_refs)?;
        self.ensure_fixed_bytes(function_refs, 4, "function references")?;
        let mut functions = Vec::with_capacity(function_refs);
        for _ in 0..function_refs {
            let function = self.u32("function reference")?;
            if function as usize >= function_count {
                return Err(Diagnostic::artifact(
                    "artifact_invalid_index",
                    format!("chunk {chunk_id} references missing function {function}"),
                ));
            }
            functions.push(function);
        }

        let local_count = self.count("local slots", self.limits.max_metadata_entries)?;
        self.budget.metadata(local_count)?;
        let mut local_slots = Vec::with_capacity(local_count);
        for _ in 0..local_count {
            local_slots.push(WireLocalSlot {
                name: self.string("local-slot name")?,
                mutable: self.boolean("local mutability")?,
                scope_depth: self.u32("local scope depth")?,
            });
        }
        let references_outer_names = self.boolean("outer-name reference flag")?;
        Ok(WireChunk {
            code,
            constants,
            lines,
            columns,
            source_file,
            functions,
            local_slots,
            references_outer_names,
        })
    }

    fn function(&mut self, function_id: usize) -> Result<WireFunction, Diagnostic> {
        let name = self.string("function name")?;
        let type_params = self.string_vec("type parameters")?;
        let nominal_type_names = self.string_vec("nominal type names")?;
        let param_count = self.count("parameters", self.limits.max_metadata_entries)?;
        self.budget.metadata(param_count)?;
        let mut params = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            let name = self.string("parameter name")?;
            let type_expr = match self.u8("parameter type presence")? {
                0 => None,
                1 => Some(self.type_expr(1)?),
                value => {
                    return Err(Diagnostic::artifact(
                        "artifact_malformed",
                        format!("function {function_id} has invalid parameter-type tag {value}"),
                    ))
                }
            };
            let has_default = self.boolean("parameter default flag")?;
            params.push(WireParam {
                name,
                type_expr,
                has_default,
            });
        }
        let default_start = match self.u8("default parameter presence")? {
            0 => None,
            1 => Some(self.u32("default parameter boundary")?),
            value => {
                return Err(Diagnostic::artifact(
                    "artifact_malformed",
                    format!("function {function_id} has invalid default-boundary tag {value}"),
                ))
            }
        };
        let chunk = self.u32("function chunk")?;
        let is_generator = self.boolean("generator flag")?;
        let is_stream = self.boolean("stream flag")?;
        let has_rest_param = self.boolean("rest-parameter flag")?;
        let has_runtime_type_checks = self.boolean("runtime-type flag")?;
        Ok(WireFunction {
            name,
            type_params,
            nominal_type_names,
            params,
            default_start,
            chunk,
            is_generator,
            is_stream,
            has_rest_param,
            has_runtime_type_checks,
        })
    }

    fn type_expr(&mut self, depth: usize) -> Result<TypeExpr, Diagnostic> {
        self.budget.type_node(depth)?;
        let tag = self.u8("parameter type tag")?;
        Ok(match tag {
            0 => TypeExpr::Named(self.string("named type")?),
            1 => TypeExpr::Union(self.type_vec(depth, "union members")?),
            2 => TypeExpr::Intersection(self.type_vec(depth, "intersection members")?),
            3 => TypeExpr::Shape(self.shape_fields(depth)?),
            4 => TypeExpr::OpenShape {
                fields: self.shape_fields(depth)?,
                rests: self.type_vec(depth, "open-shape rests")?,
            },
            5 => TypeExpr::List(Box::new(self.type_expr(depth + 1)?)),
            6 => TypeExpr::Tuple(self.type_vec(depth, "tuple members")?),
            7 => TypeExpr::DictType(
                Box::new(self.type_expr(depth + 1)?),
                Box::new(self.type_expr(depth + 1)?),
            ),
            8 => TypeExpr::Iter(Box::new(self.type_expr(depth + 1)?)),
            9 => TypeExpr::Generator(Box::new(self.type_expr(depth + 1)?)),
            10 => TypeExpr::Stream(Box::new(self.type_expr(depth + 1)?)),
            11 => TypeExpr::Owned(Box::new(self.type_expr(depth + 1)?)),
            12 => TypeExpr::Applied {
                name: self.string("applied type name")?,
                args: self.type_vec(depth, "applied type arguments")?,
            },
            13 => TypeExpr::FnType {
                params: self.type_vec(depth, "function type parameters")?,
                return_type: Box::new(self.type_expr(depth + 1)?),
            },
            14 => TypeExpr::Never,
            15 => TypeExpr::LitString(self.string("literal string type")?),
            16 => TypeExpr::LitInt(self.i64("literal integer type")?),
            value => {
                return Err(Diagnostic::artifact(
                    "artifact_malformed",
                    format!("artifact has invalid parameter-type tag {value}"),
                ))
            }
        })
    }

    fn type_vec(&mut self, depth: usize, kind: &str) -> Result<Vec<TypeExpr>, Diagnostic> {
        let count = self.count(kind, self.limits.max_metadata_entries)?;
        self.budget.metadata(count)?;
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(self.type_expr(depth + 1)?);
        }
        Ok(values)
    }

    fn shape_fields(&mut self, depth: usize) -> Result<Vec<ShapeField>, Diagnostic> {
        let count = self.count("shape fields", self.limits.max_metadata_entries)?;
        self.budget.metadata(count)?;
        let mut fields = Vec::with_capacity(count);
        for _ in 0..count {
            let name = self.string("shape field name")?;
            let type_expr = self.type_expr(depth + 1)?;
            let optional = self.boolean("shape field optional flag")?;
            fields.push(ShapeField::synthetic(name, type_expr, optional));
        }
        Ok(fields)
    }

    fn constant(&mut self) -> Result<Constant, Diagnostic> {
        Ok(match self.u8("constant tag")? {
            0 => Constant::Int(self.i64("integer constant")?),
            1 => Constant::Float(f64::from_bits(self.u64("float constant")?)),
            2 => Constant::String(self.string("string constant")?),
            3 => Constant::Bool(self.boolean("boolean constant")?),
            4 => Constant::Nil,
            5 => Constant::Duration(self.i64("duration constant")?),
            value => {
                return Err(Diagnostic::artifact(
                    "artifact_malformed",
                    format!("artifact has invalid constant tag {value}"),
                ))
            }
        })
    }

    fn string_vec(&mut self, kind: &str) -> Result<Vec<String>, Diagnostic> {
        let count = self.count(kind, self.limits.max_metadata_entries)?;
        self.budget.metadata(count)?;
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(self.string(kind)?);
        }
        Ok(values)
    }

    fn u32_vec_exact(&mut self, expected: usize, kind: &str) -> Result<Vec<u32>, Diagnostic> {
        let count = self.count(kind, self.limits.max_instructions)?;
        if count != expected {
            return Err(Diagnostic::artifact(
                "artifact_malformed",
                format!("artifact {kind} count {count} does not match code length {expected}"),
            ));
        }
        self.ensure_fixed_bytes(count, 4, kind)?;
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(self.u32(kind)?);
        }
        Ok(values)
    }

    fn optional_string(&mut self, kind: &str) -> Result<Option<String>, Diagnostic> {
        match self.u8(kind)? {
            0 => Ok(None),
            1 => self.string(kind).map(Some),
            value => Err(Diagnostic::artifact(
                "artifact_malformed",
                format!("artifact {kind} has invalid option tag {value}"),
            )),
        }
    }

    fn string(&mut self, kind: &str) -> Result<String, Diagnostic> {
        let len = self.count(kind, self.limits.max_string_bytes)?;
        let bytes = self.take(len, kind)?;
        let value = std::str::from_utf8(bytes).map_err(|_| {
            Diagnostic::artifact(
                "artifact_invalid_utf8",
                format!("artifact {kind} is not valid UTF-8"),
            )
        })?;
        self.budget.string(value)?;
        Ok(value.to_owned())
    }

    fn count(&mut self, kind: &str, limit: usize) -> Result<usize, Diagnostic> {
        let value = self.u32(kind)? as usize;
        if value > limit {
            return Err(Diagnostic::artifact(
                "artifact_allocation_limit",
                format!("artifact {kind} count {value} exceeds limit {limit}"),
            ));
        }
        Ok(value)
    }

    fn boolean(&mut self, kind: &str) -> Result<bool, Diagnostic> {
        match self.u8(kind)? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(Diagnostic::artifact(
                "artifact_malformed",
                format!("artifact {kind} has invalid boolean {value}"),
            )),
        }
    }

    fn u8(&mut self, kind: &str) -> Result<u8, Diagnostic> {
        Ok(self.take(1, kind)?[0])
    }

    fn u32(&mut self, kind: &str) -> Result<u32, Diagnostic> {
        Ok(u32::from_be_bytes(
            self.take(4, kind)?.try_into().expect("fixed-size u32"),
        ))
    }

    fn u64(&mut self, kind: &str) -> Result<u64, Diagnostic> {
        Ok(u64::from_be_bytes(
            self.take(8, kind)?.try_into().expect("fixed-size u64"),
        ))
    }

    fn i64(&mut self, kind: &str) -> Result<i64, Diagnostic> {
        Ok(i64::from_be_bytes(
            self.take(8, kind)?.try_into().expect("fixed-size i64"),
        ))
    }

    fn ensure_fixed_bytes(&self, count: usize, width: usize, kind: &str) -> Result<(), Diagnostic> {
        let bytes = count.checked_mul(width).ok_or_else(|| {
            Diagnostic::artifact(
                "artifact_too_large",
                format!("artifact {kind} size overflows"),
            )
        })?;
        if bytes > self.bytes.len().saturating_sub(self.offset) {
            return Err(Diagnostic::artifact(
                "artifact_truncated",
                format!("artifact {kind} is truncated"),
            ));
        }
        Ok(())
    }

    fn take(&mut self, len: usize, kind: &str) -> Result<&'a [u8], Diagnostic> {
        let end = self.offset.checked_add(len).ok_or_else(|| {
            Diagnostic::artifact(
                "artifact_too_large",
                format!("artifact {kind} size overflows"),
            )
        })?;
        let value = self.bytes.get(self.offset..end).ok_or_else(|| {
            Diagnostic::artifact(
                "artifact_truncated",
                format!("artifact {kind} is truncated"),
            )
        })?;
        self.offset = end;
        Ok(value)
    }
}
