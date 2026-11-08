//! Final path denials hold even when a preset or explicit grant is broader.

use super::{sandbox_profile_escape, sandbox_profile_path_aliases};

pub(super) fn append_boundary(profile: &mut String) {
    let Some(boundary) = super::super::replay::boundary() else {
        return;
    };
    append_boundary_for(profile, &boundary);
}

fn append_boundary_for(profile: &mut String, boundary: &super::super::replay::FilesystemBoundary) {
    let mut pinned = std::collections::BTreeSet::new();
    for root in &boundary.recorded {
        for path in sandbox_profile_path_aliases(&root.display().to_string()) {
            deny(
                profile,
                &format!("(subpath \"{}\")", sandbox_profile_escape(&path)),
            );
            pin_ancestors(&mut pinned, &path);
        }
    }
    let workspaces = sandbox_profile_path_aliases(&boundary.workspace.display().to_string());
    for path in sandbox_profile_path_aliases(&boundary.container.display().to_string()) {
        let mut filter = format!(
            "(require-all (subpath \"{}\")",
            sandbox_profile_escape(&path)
        );
        for workspace in &workspaces {
            filter.push_str(&format!(
                " (require-not (subpath \"{}\"))",
                sandbox_profile_escape(workspace),
            ));
        }
        filter.push(')');
        deny(profile, &filter);
        pin_ancestors(&mut pinned, &path);
    }
    for path in pinned {
        profile.push_str(&format!(
            "(deny file-write-unlink (literal \"{}\"))\n",
            sandbox_profile_escape(&path),
        ));
    }
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;

fn deny(profile: &mut String, filter: &str) {
    profile.push_str(&format!(
        "(deny file-read* file-write* {filter})\n\
         (deny network-bind network-inbound network-outbound {filter})\n",
    ));
}

fn pin_ancestors(pinned: &mut std::collections::BTreeSet<String>, path: &str) {
    pinned.extend(
        std::path::Path::new(path)
            .ancestors()
            .skip(1)
            .filter(|ancestor| ancestor.parent().is_some())
            .map(|ancestor| ancestor.display().to_string()),
    );
}
