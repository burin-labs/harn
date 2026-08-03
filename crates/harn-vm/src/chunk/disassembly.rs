//! Operand-layout renderers for bytecode disassembly.
//!
//! The opcode registry chooses one of these helpers for each instruction. A
//! helper advances past exactly the operands described by that layout and
//! returns one human-readable line. The portable-to-native adapter separately
//! uses `harn_kernel::program::instruction_len`, so it does not duplicate an
//! opcode-width table.

use super::Chunk;

pub(crate) fn disasm_bare(_chunk: &Chunk, _ip: &mut usize, label: &str) -> String {
    label.to_string()
}

pub(crate) fn disasm_u8(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let arg = chunk.code[*ip];
    *ip += 1;
    format!("{label} {arg:>4}")
}

pub(crate) fn disasm_u16(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let arg = chunk.read_u16(*ip);
    *ip += 2;
    format!("{label} {arg:>4}")
}

pub(crate) fn disasm_try_catch_setup(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let catch_offset = chunk.read_u16(*ip);
    *ip += 2;
    let type_idx = chunk.read_u16(*ip);
    *ip += 2;
    if let Some(type_name) = chunk.constants.get(type_idx as usize) {
        format!("{label} {catch_offset:>4} type {type_idx:>4} ({type_name})")
    } else {
        format!("{label} {catch_offset:>4} type {type_idx:>4}")
    }
}

pub(crate) fn disasm_const_pool_u16(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let idx = chunk.read_u16(*ip);
    *ip += 2;
    format!("{label} {idx:>4} ({})", chunk.constants[idx as usize])
}

pub(crate) fn disasm_local_slot_u16(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let slot = chunk.read_u16(*ip);
    *ip += 2;
    let mut out = format!("{label} {slot:>4}");
    if let Some(info) = chunk.local_slots.get(slot as usize) {
        out.push_str(&format!(" ({})", info.name));
    }
    out
}

pub(crate) fn disasm_const_pool_local_slot(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let prop = chunk.read_u16(*ip);
    *ip += 2;
    let slot = chunk.read_u16(*ip);
    *ip += 2;
    let mut out = format!(
        "{label} prop {prop:>4} ({}) slot {slot:>4}",
        chunk.constants[prop as usize]
    );
    if let Some(info) = chunk.local_slots.get(slot as usize) {
        out.push_str(&format!(" ({})", info.name));
    }
    out
}

pub(crate) fn disasm_method_call(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let idx = chunk.read_u16(*ip);
    *ip += 2;
    let argc = chunk.code[*ip];
    *ip += 1;
    format!(
        "{label} {idx:>4} ({}) argc={argc}",
        chunk.constants[idx as usize]
    )
}

pub(crate) fn disasm_match_enum(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let enum_idx = chunk.read_u16(*ip);
    *ip += 2;
    let var_idx = chunk.read_u16(*ip);
    *ip += 2;
    format!(
        "{label} {enum_idx:>4} ({}) {var_idx:>4} ({})",
        chunk.constants[enum_idx as usize], chunk.constants[var_idx as usize],
    )
}

pub(crate) fn disasm_build_enum(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let enum_idx = chunk.read_u16(*ip);
    *ip += 2;
    let var_idx = chunk.read_u16(*ip);
    *ip += 2;
    let field_count = chunk.read_u16(*ip);
    *ip += 2;
    format!(
        "{label} {enum_idx:>4} ({}) {var_idx:>4} ({}) fields={field_count}",
        chunk.constants[enum_idx as usize], chunk.constants[var_idx as usize],
    )
}

pub(crate) fn disasm_selective_import(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let path_idx = chunk.read_u16(*ip);
    *ip += 2;
    let names_idx = chunk.read_u16(*ip);
    *ip += 2;
    format!(
        "{label} {path_idx:>4} ({}) names: {names_idx:>4} ({})",
        chunk.constants[path_idx as usize], chunk.constants[names_idx as usize],
    )
}

pub(crate) fn disasm_namespace_import_members(
    chunk: &Chunk,
    ip: &mut usize,
    label: &str,
) -> String {
    let path_idx = chunk.read_u16(*ip);
    *ip += 2;
    let alias_idx = chunk.read_u16(*ip);
    *ip += 2;
    let names_idx = chunk.read_u16(*ip);
    *ip += 2;
    format!(
        "{label} {path_idx:>4} ({}) alias: {alias_idx:>4} ({}) members: {names_idx:>4} ({})",
        chunk.constants[path_idx as usize],
        chunk.constants[alias_idx as usize],
        chunk.constants[names_idx as usize],
    )
}

pub(crate) fn disasm_check_type(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let var_idx = chunk.read_u16(*ip);
    *ip += 2;
    let type_idx = chunk.read_u16(*ip);
    *ip += 2;
    format!(
        "{label} {var_idx:>4} ({}) -> {type_idx:>4} ({})",
        chunk.constants[var_idx as usize], chunk.constants[type_idx as usize],
    )
}

pub(crate) fn disasm_call_builtin(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let id = chunk.read_u64(*ip);
    *ip += 8;
    let idx = chunk.read_u16(*ip);
    *ip += 2;
    let argc = chunk.code[*ip];
    *ip += 1;
    format!(
        "{label} {id:#018x} {idx:>4} ({}) argc={argc}",
        chunk.constants[idx as usize],
    )
}

pub(crate) fn disasm_call_builtin_spread(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    let id = chunk.read_u64(*ip);
    *ip += 8;
    let idx = chunk.read_u16(*ip);
    *ip += 2;
    format!(
        "{label} {id:#018x} {idx:>4} ({})",
        chunk.constants[idx as usize],
    )
}

pub(crate) fn disasm_method_call_spread(chunk: &Chunk, ip: &mut usize, label: &str) -> String {
    // emit_u16(Op::MethodCallSpread, name_idx, ...) writes opcode + 2
    // bytes of u16 name_idx, so the operand is read at *ip with the
    // usual `read_u16`.
    let idx = chunk.read_u16(*ip);
    *ip += 2;
    format!("{label} {idx:>4} ({})", chunk.constants[idx as usize])
}
