use serde_json::{json, Value as JsonValue};

use crate::value::VmError;

use super::{bool_option, string_list_option, string_option};

pub(super) fn tag_list_argv(options: Option<&crate::value::DictMap>) -> Vec<String> {
    let mut argv = vec!["git".to_string(), "tag".to_string(), "--list".to_string()];
    if let Some(sort) = string_option(options, "sort") {
        argv.push(format!("--sort={sort}"));
    }
    if let Some(pattern) = string_option(options, "pattern") {
        argv.push("--".to_string());
        argv.push(pattern);
    }
    argv
}

pub(super) fn describe_argv(options: Option<&crate::value::DictMap>) -> Vec<String> {
    let mut argv = vec![
        "git".to_string(),
        "describe".to_string(),
        "--long".to_string(),
        "--always".to_string(),
        "--dirty".to_string(),
    ];
    if bool_option(options, "tags").unwrap_or(false) {
        argv.push("--tags".to_string());
    }
    if let Some(pattern) = string_option(options, "match") {
        argv.push("--match".to_string());
        argv.push(pattern);
    }
    if let Some(rev) = string_option(options, "rev") {
        argv.push("--".to_string());
        argv.push(rev);
    }
    argv
}

pub(super) fn ls_remote_argv(
    remote: &str,
    options: Option<&crate::value::DictMap>,
) -> Result<Vec<String>, VmError> {
    let mut argv = vec!["git".to_string(), "ls-remote".to_string()];
    if bool_option(options, "tags").unwrap_or(false) {
        argv.push("--tags".to_string());
    }
    if bool_option(options, "heads").unwrap_or(false) {
        argv.push("--heads".to_string());
    }
    argv.push("--".to_string());
    argv.push(remote.to_string());
    if let Some(refs) = string_list_option(options, "refs", "git.ls_remote")? {
        argv.extend(refs);
    }
    Ok(argv)
}

pub(super) fn parse_tag_list(stdout: &str) -> JsonValue {
    let tags = stdout
        .lines()
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>();
    json!({"tags": tags})
}

pub(super) fn parse_describe(stdout: &str) -> JsonValue {
    let describe = stdout.trim();
    let (value, dirty) = describe
        .strip_suffix("-dirty")
        .map_or((describe, false), |value| (value, true));
    let mut suffix = value.rsplitn(3, '-');
    let sha_part = suffix.next().unwrap_or("");
    let distance_part = suffix.next().unwrap_or("");
    let tag_part = suffix.next();
    let parsed = tag_part
        .zip(sha_part.strip_prefix('g'))
        .and_then(|(tag, sha)| {
            distance_part
                .parse::<u64>()
                .ok()
                .map(|distance| (tag, sha, distance))
        });
    if let Some((tag, sha, distance)) = parsed {
        return json!({
            "describe": describe,
            "tag": tag,
            "distance": distance,
            "sha": sha,
            "dirty": dirty,
        });
    }
    json!({
        "describe": describe,
        "tag": JsonValue::Null,
        "distance": JsonValue::Null,
        "sha": value,
        "dirty": dirty,
    })
}

pub(super) fn parse_ls_remote(stdout: &str, remote: &str) -> JsonValue {
    let refs = stdout
        .lines()
        .filter_map(|line| {
            let (oid, ref_name) = line.split_once('\t')?;
            let peeled = ref_name.ends_with("^{}");
            Some(json!({
                "oid": oid,
                "ref": ref_name,
                "name": ref_name.strip_suffix("^{}").unwrap_or(ref_name),
                "peeled": peeled,
            }))
        })
        .collect::<Vec<_>>();
    json!({"remote": remote, "refs": refs})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsers_preserve_hyphenated_tags_and_peeled_refs() {
        assert_eq!(
            parse_tag_list("v2.0.0\nv1.9.0\n\n"),
            json!({"tags": ["v2.0.0", "v1.9.0"]})
        );
        assert_eq!(
            parse_describe("release-candidate-2-7-gabc123-dirty\n"),
            json!({
                "describe": "release-candidate-2-7-gabc123-dirty",
                "tag": "release-candidate-2",
                "distance": 7,
                "sha": "abc123",
                "dirty": true,
            })
        );
        assert_eq!(
            parse_describe("abc123\n"),
            json!({
                "describe": "abc123",
                "tag": JsonValue::Null,
                "distance": JsonValue::Null,
                "sha": "abc123",
                "dirty": false,
            })
        );
        assert_eq!(
            parse_ls_remote(
                "aaaaaaaa\trefs/tags/v1.0.0\nbbbbbbbb\trefs/tags/v1.0.0^{}\n",
                "origin"
            ),
            json!({
                "remote": "origin",
                "refs": [
                    {"oid": "aaaaaaaa", "ref": "refs/tags/v1.0.0", "name": "refs/tags/v1.0.0", "peeled": false},
                    {"oid": "bbbbbbbb", "ref": "refs/tags/v1.0.0^{}", "name": "refs/tags/v1.0.0", "peeled": true},
                ],
            })
        );
    }

    #[test]
    fn argv_uses_option_boundaries() {
        let tag_options = crate::stdlib::json_to_vm_value(&json!({
            "pattern": "--contains=surprise",
            "sort": "-v:refname",
        }));
        assert_eq!(
            tag_list_argv(tag_options.as_dict()),
            [
                "git",
                "tag",
                "--list",
                "--sort=-v:refname",
                "--",
                "--contains=surprise"
            ]
        );

        let describe_options = crate::stdlib::json_to_vm_value(&json!({
            "tags": true,
            "match": "v*",
            "rev": "--all",
        }));
        assert_eq!(
            describe_argv(describe_options.as_dict()),
            [
                "git", "describe", "--long", "--always", "--dirty", "--tags", "--match", "v*",
                "--", "--all"
            ]
        );

        let remote_options = crate::stdlib::json_to_vm_value(&json!({
            "tags": true,
            "refs": ["--upload-pack=surprise"],
        }));
        assert_eq!(
            ls_remote_argv("--exec=surprise", remote_options.as_dict()).expect("valid refs"),
            [
                "git",
                "ls-remote",
                "--tags",
                "--",
                "--exec=surprise",
                "--upload-pack=surprise"
            ]
        );
    }
}
