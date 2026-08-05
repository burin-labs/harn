use harn_parser::{ShapeField, TypeExpr};

use crate::Constant;

use super::WireProgram;
use crate::artifact::{Diagnostic, EntryKind};

pub(in crate::artifact) fn encode_wire_program(
    program: &WireProgram,
) -> Result<Vec<u8>, Diagnostic> {
    let mut writer = ArtifactWriter::new();
    writer.bytes(&program.semantic_abi);
    writer.string(&program.entry)?;
    writer.u8(match program.entry_kind {
        EntryKind::Function => 0,
        EntryKind::Pipeline => 1,
    });
    writer.boolean(program.expects_harness);
    writer.len(program.chunks.len(), "chunks")?;
    writer.len(program.functions.len(), "functions")?;
    for chunk in &program.chunks {
        writer.byte_vec(&chunk.code, "instruction bytes")?;
        writer.len(chunk.constants.len(), "constants")?;
        for constant in &chunk.constants {
            writer.constant(constant)?;
        }
        writer.u32_vec(&chunk.lines, "source lines")?;
        writer.u32_vec(&chunk.columns, "source columns")?;
        writer.optional_string(chunk.source_file.as_deref())?;
        writer.len(chunk.functions.len(), "function references")?;
        for function in &chunk.functions {
            writer.u32(*function);
        }
        writer.len(chunk.local_slots.len(), "local slots")?;
        for slot in &chunk.local_slots {
            writer.string(&slot.name)?;
            writer.boolean(slot.mutable);
            writer.u32(slot.scope_depth);
        }
        writer.boolean(chunk.references_outer_names);
    }
    for function in &program.functions {
        writer.string(&function.name)?;
        writer.string_vec(&function.type_params, "type parameters")?;
        writer.string_vec(&function.nominal_type_names, "nominal type names")?;
        writer.len(function.params.len(), "parameters")?;
        for param in &function.params {
            writer.string(&param.name)?;
            match &param.type_expr {
                Some(type_expr) => {
                    writer.u8(1);
                    writer.type_expr(type_expr)?;
                }
                None => writer.u8(0),
            }
            writer.boolean(param.has_default);
        }
        match function.default_start {
            Some(start) => {
                writer.u8(1);
                writer.u32(start);
            }
            None => writer.u8(0),
        }
        writer.u32(function.chunk);
        writer.boolean(function.is_generator);
        writer.boolean(function.is_stream);
        writer.boolean(function.has_rest_param);
        writer.boolean(function.has_runtime_type_checks);
    }
    writer.import_vec(&program.root_imports)?;
    writer.len(program.modules.len(), "modules")?;
    for module in &program.modules {
        writer.string(&module.id)?;
        writer.import_vec(&module.imports)?;
        match module.init {
            Some(chunk) => {
                writer.u8(1);
                writer.u32(chunk);
            }
            None => writer.u8(0),
        }
        writer.len(module.functions.len(), "module functions")?;
        for (name, function) in &module.functions {
            writer.string(name)?;
            writer.u32(*function);
        }
        writer.len(module.exports.len(), "module exports")?;
        for (name, kind) in &module.exports {
            writer.string(name)?;
            writer.u8(export_kind_tag(*kind));
        }
    }
    Ok(writer.finish())
}

fn export_kind_tag(kind: crate::PortableExportKind) -> u8 {
    match kind {
        crate::PortableExportKind::Function => 0,
        crate::PortableExportKind::Pipeline => 1,
        crate::PortableExportKind::Tool => 2,
        crate::PortableExportKind::Skill => 3,
        crate::PortableExportKind::EvalPack => 4,
        crate::PortableExportKind::Struct => 5,
        crate::PortableExportKind::Enum => 6,
        crate::PortableExportKind::Interface => 7,
        crate::PortableExportKind::Type => 8,
        crate::PortableExportKind::Variable => 9,
    }
}

struct ArtifactWriter {
    bytes: Vec<u8>,
}

impl ArtifactWriter {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn boolean(&mut self, value: bool) {
        self.u8(value.into());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn len(&mut self, value: usize, kind: &str) -> Result<(), Diagnostic> {
        let value = u32::try_from(value).map_err(|_| {
            Diagnostic::artifact(
                "artifact_too_large",
                format!("artifact has too many {kind} for the portable u32 format"),
            )
        })?;
        self.u32(value);
        Ok(())
    }

    fn string(&mut self, value: &str) -> Result<(), Diagnostic> {
        self.len(value.len(), "string bytes")?;
        self.bytes(value.as_bytes());
        Ok(())
    }

    fn optional_string(&mut self, value: Option<&str>) -> Result<(), Diagnostic> {
        match value {
            Some(value) => {
                self.u8(1);
                self.string(value)
            }
            None => {
                self.u8(0);
                Ok(())
            }
        }
    }

    fn byte_vec(&mut self, value: &[u8], kind: &str) -> Result<(), Diagnostic> {
        self.len(value.len(), kind)?;
        self.bytes(value);
        Ok(())
    }

    fn u32_vec(&mut self, value: &[u32], kind: &str) -> Result<(), Diagnostic> {
        self.len(value.len(), kind)?;
        for item in value {
            self.u32(*item);
        }
        Ok(())
    }

    fn string_vec(&mut self, value: &[String], kind: &str) -> Result<(), Diagnostic> {
        self.len(value.len(), kind)?;
        for item in value {
            self.string(item)?;
        }
        Ok(())
    }

    fn import_vec(&mut self, imports: &[crate::PortableImport]) -> Result<(), Diagnostic> {
        self.len(imports.len(), "imports")?;
        for import in imports {
            self.string(&import.path)?;
            self.string(&import.target)?;
            match &import.selected_names {
                Some(names) => {
                    self.u8(1);
                    self.string_vec(names, "selected import names")?;
                }
                None => self.u8(0),
            }
            match &import.namespace_alias {
                Some(alias) => {
                    self.u8(1);
                    self.string(alias)?;
                }
                None => self.u8(0),
            }
            self.boolean(import.is_pub);
        }
        Ok(())
    }

    fn constant(&mut self, constant: &Constant) -> Result<(), Diagnostic> {
        match constant {
            Constant::Int(value) => {
                self.u8(0);
                self.i64(*value);
            }
            Constant::Float(value) => {
                self.u8(1);
                self.u64(value.to_bits());
            }
            Constant::String(value) => {
                self.u8(2);
                self.string(value)?;
            }
            Constant::Bool(value) => {
                self.u8(3);
                self.boolean(*value);
            }
            Constant::Nil => self.u8(4),
            Constant::Duration(value) => {
                self.u8(5);
                self.i64(*value);
            }
        }
        Ok(())
    }

    fn type_expr(&mut self, type_expr: &TypeExpr) -> Result<(), Diagnostic> {
        match type_expr {
            TypeExpr::Named(name) => {
                self.u8(0);
                self.string(name)?;
            }
            TypeExpr::Union(items) => {
                self.u8(1);
                self.type_vec(items, "union members")?;
            }
            TypeExpr::Intersection(items) => {
                self.u8(2);
                self.type_vec(items, "intersection members")?;
            }
            TypeExpr::Shape(fields) => {
                self.u8(3);
                self.shape_fields(fields)?;
            }
            TypeExpr::OpenShape { fields, rests } => {
                self.u8(4);
                self.shape_fields(fields)?;
                self.type_vec(rests, "open-shape rests")?;
            }
            TypeExpr::List(inner) => {
                self.u8(5);
                self.type_expr(inner)?;
            }
            TypeExpr::Tuple(items) => {
                self.u8(6);
                self.type_vec(items, "tuple members")?;
            }
            TypeExpr::DictType(key, value) => {
                self.u8(7);
                self.type_expr(key)?;
                self.type_expr(value)?;
            }
            TypeExpr::Iter(inner) => {
                self.u8(8);
                self.type_expr(inner)?;
            }
            TypeExpr::Generator(inner) => {
                self.u8(9);
                self.type_expr(inner)?;
            }
            TypeExpr::Stream(inner) => {
                self.u8(10);
                self.type_expr(inner)?;
            }
            TypeExpr::Owned(inner) => {
                self.u8(11);
                self.type_expr(inner)?;
            }
            TypeExpr::Applied { name, args } => {
                self.u8(12);
                self.string(name)?;
                self.type_vec(args, "applied type arguments")?;
            }
            TypeExpr::FnType {
                params,
                return_type,
            } => {
                self.u8(13);
                self.type_vec(params, "function type parameters")?;
                self.type_expr(return_type)?;
            }
            TypeExpr::Never => self.u8(14),
            TypeExpr::LitString(value) => {
                self.u8(15);
                self.string(value)?;
            }
            TypeExpr::LitInt(value) => {
                self.u8(16);
                self.i64(*value);
            }
        }
        Ok(())
    }

    fn type_vec(&mut self, values: &[TypeExpr], kind: &str) -> Result<(), Diagnostic> {
        self.len(values.len(), kind)?;
        for value in values {
            self.type_expr(value)?;
        }
        Ok(())
    }

    fn shape_fields(&mut self, fields: &[ShapeField]) -> Result<(), Diagnostic> {
        self.len(fields.len(), "shape fields")?;
        for field in fields {
            self.string(&field.name)?;
            self.type_expr(&field.type_expr)?;
            self.boolean(field.optional);
        }
        Ok(())
    }
}
