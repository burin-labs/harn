use std::collections::HashSet;

/// Core builtins that are never denied, even when using `--allow`.
const CORE_BUILTINS: &[&str] = &[
    "println",
    "print",
    "log",
    "type_of",
    "to_string",
    "to_int",
    "to_float",
    "len",
    "assert",
    "assert_eq",
    "assert_ne",
    "json_parse",
    "json_stringify",
    "runtime_context",
    "task_current",
    "runtime_context_values",
    "runtime_context_get",
    "runtime_context_set",
    "runtime_context_clear",
];

/// Build the set of denied builtin names from `--deny` or `--allow` flags.
///
/// `--deny` denies the named families. `--allow` preserves the named families,
/// core builtins, and all pure builtins, whose registry contracts have no effects.
pub(crate) fn build_denied_builtins(
    deny_csv: Option<&str>,
    allow_csv: Option<&str>,
) -> HashSet<String> {
    if let Some(csv) = deny_csv {
        csv.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else if let Some(csv) = allow_csv {
        let allowed: HashSet<String> = csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let mut core: HashSet<&str> = CORE_BUILTINS.iter().copied().collect();
        core.extend(
            harn_vm::stdlib::all_builtin_defs()
                .iter()
                .filter(|def| {
                    matches!(
                        def.contract.exposure,
                        harn_builtin_meta::BuiltinExposure::PureGlobal
                    )
                })
                .flat_map(|def| std::iter::once(def.sig.name).chain(def.aliases.iter().copied())),
        );

        let mut tmp = harn_vm::new_vm();
        harn_runtime::register_vm_stdlib(&mut tmp);
        harn_vm::register_store_builtins(&mut tmp, std::path::Path::new("."));
        harn_vm::register_metadata_builtins(&mut tmp, std::path::Path::new("."));

        tmp.builtin_names()
            .into_iter()
            .filter(|name| !allowed.contains(name) && !core.contains(name.as_str()))
            .collect()
    } else {
        HashSet::new()
    }
}
