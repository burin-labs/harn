//! `find` deletes through `-delete` and `-exec rm`, so its starting points
//! face the same escape and project-wipe floor as `rm -rf` targets.

use super::*;

#[test]
fn floor_blocks_find_deleting_outside_the_workspace_or_the_whole_root() {
    for cmd in [
        "find / -delete",
        "find /etc -name '*.bak' -delete",
        "find ~ -type f -delete",
        "find .. -delete",
        "find -L /var/data -exec rm -rf {} +",
        "find / -execdir rm {} \\;",
        "find /tmp/other -ok rm {} \\;",
        "find /var/data -exec sudo rm -f {} +",
        "sudo find / -delete",
        "find . -delete",
        "find -delete",
        "find . -mindepth 1 -delete",
        "find ./ -exec rm -rf {} +",
        "find . -exec rm -rf {} \\; -name '*.tmp'",
        "find . -false -o -delete",
        "find . -name keep -o -delete",
        "find . ! -name keep -delete",
        "find . -true -delete",
        "find . -printf '%p\\n' -delete",
        "find . -fprint /dev/null -delete",
        "find . -prune -delete",
        "find . -printf -name -delete",
        "find . -fprintf /dev/null -name -delete",
        "find -O3 / -name '*.bak' -delete",
        "find -D tree / -name '*.bak' -delete",
        "find -H -L /etc -delete",
        "find -files0-from list.txt -name x -delete",
    ] {
        assert!(
            is_cat_root(cmd),
            "expected catastrophic (find delete): {cmd}"
        );
    }
}

#[test]
fn floor_allows_scoped_and_read_only_finds() {
    for cmd in [
        "find . -name '*.o' -delete",
        "find build -delete",
        "find /home/dev/project/build -delete",
        "find src -type f -exec rm {} +",
        "find / -name passwd",
        "find / -maxdepth 1 -print",
        "find / -exec grep -l secret {} +",
    ] {
        assert!(!is_cat_root(cmd), "should NOT be catastrophic: {cmd}");
    }
}
