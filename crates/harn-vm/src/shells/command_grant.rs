//! Shell-mode commands that a command-bound grant names.
//!
//! A `for_command` grant matches the spawned executable. A shell-mode command
//! spawns the shell, so `gh issue list` run through `sh -c` would never match
//! a `for_command: "gh"` grant, and granting the shell instead would hand the
//! credential to everything the script runs. When the script is exactly one
//! plain external command whose program a command-bound grant names, Harn
//! spawns that argv directly, so the granted process is the only one that
//! exists. Everything else keeps the shell and receives no command-bound
//! grant.

use super::{plan_posix_command, shell_dialect_for_id, ShellDialect, ShellInvocation};

impl ShellInvocation {
    /// The argv to spawn in place of this shell invocation, when its command
    /// is one plain external command a command-bound grant names.
    ///
    /// The command must parse, as POSIX shell syntax, to a single simple
    /// command with literal words only: no list, pipeline, redirect,
    /// assignment prefix, expansion, substitution, glob, subshell, or
    /// background operator (see [`plan_posix_command`]). The program must be
    /// a bare name looked up on the child's `PATH`, never a path a workspace
    /// file could occupy. Any doubt keeps the shell.
    pub fn command_grant_argv(&self) -> Option<Vec<String>> {
        let argv = self.direct_argv_candidate()?;
        crate::stdlib::process::command_bound_grant_reaches(&argv[0]).then_some(argv)
    }

    fn direct_argv_candidate(&self) -> Option<Vec<String>> {
        if shell_dialect_for_id(&self.program) != Some(ShellDialect::Posix) {
            return None;
        }
        // Only `[flags..., -c, command]`: anything after the command would be
        // positional parameters the direct spawn cannot honor.
        if self.command_arg_index + 1 != self.args.len()
            || self.args[..self.command_arg_index]
                .last()
                .map(String::as_str)
                != Some("-c")
        {
            return None;
        }
        let argv = plan_posix_command(&self.args[self.command_arg_index])?.argv?;
        let program = argv.first()?;
        if program.is_empty() || program.contains(['/', '\\', '=']) {
            return None;
        }
        Some(argv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(command: &str) -> ShellInvocation {
        let shell = super::super::resolve_shell_from_vm_params(&{
            let mut params = crate::value::DictMap::new();
            let mut shell = crate::value::DictMap::new();
            crate::value::VmDictExt::put_str(&mut shell, "path", "/bin/sh");
            params.insert(
                crate::value::intern_key("shell"),
                crate::value::VmValue::dict(shell),
            );
            params
        })
        .unwrap();
        super::super::invocation_for_shell(shell, command.to_string(), false, false)
    }

    #[test]
    fn only_one_plain_command_is_a_direct_spawn_candidate() {
        assert_eq!(
            invocation("gh issue list --limit 'a b'").direct_argv_candidate(),
            Some(
                ["gh", "issue", "list", "--limit", "a b"]
                    .map(String::from)
                    .to_vec()
            )
        );
        for compound in [
            "env",
            "sh -c 'echo $GH_TOKEN'",
            "gh issue list; env",
            "gh issue list && env",
            "gh issue list || env",
            "gh issue list | env",
            "gh issue list &",
            "GH_PAGER=env gh issue list",
            "gh issue list $(env)",
            "gh issue list `env`",
            "gh issue list > out",
            "gh issue list < in",
            "gh issue list *",
            "gh issue list ~",
            "gh issue list $HOME",
            "gh issue list ${HOME}",
            "(gh issue list)",
            "{ gh issue list; }",
            "gh issue list\nenv",
            "gh issue list # comment",
            "./gh issue list",
            "/usr/bin/gh issue list",
            "env gh issue list",
            "exec gh issue list",
            "gh issue list <<< in",
            "gh issue list 'unterminated",
        ] {
            let candidate = invocation(compound).direct_argv_candidate();
            assert!(
                candidate.as_ref().is_none_or(|argv| argv[0] != "gh"),
                "{compound:?} must keep the shell, planned {candidate:?}"
            );
        }
    }
}
