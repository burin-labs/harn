"""Directory-FD operations for the target collector's incremental-only action.

The shell collector owns selection, protection and process/lease admission.
This module only stages, restores or deletes inode-bound Cargo cache parts.
"""

import argparse
from contextlib import ExitStack
import json
import inspect
import os
from pathlib import Path
import shutil
import stat
import uuid


PREFIX = ".harn-gc-incremental-"
SCHEMA = "harn-target-gc-incremental/v1"
DIRECTORY = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW


class Refusal(Exception):
    pass


def identity(fd):
    value = os.fstat(fd)
    return [value.st_dev, value.st_ino]


def open_dir(stack, name, parent=None):
    fd = os.open(name, DIRECTORY, dir_fd=parent)
    stack.callback(os.close, fd)
    return fd


def component(name):
    if not name or name in (".", "..") or "/" in name or any(ord(c) < 32 for c in name):
        raise Refusal("invalid path component")
    return name


def optional_stat(fd, name):
    try:
        return os.stat(name, dir_fd=fd, follow_symlinks=False)
    except FileNotFoundError:
        return None


def allocated_bytes(fd, seen=None):
    seen = set() if seen is None else seen
    total = os.fstat(fd).st_blocks * 512
    for name in os.listdir(fd):
        value = os.stat(name, dir_fd=fd, follow_symlinks=False)
        key = (value.st_dev, value.st_ino)
        if key in seen:
            continue
        seen.add(key)
        if stat.S_ISDIR(value.st_mode):
            with ExitStack() as stack:
                child = open_dir(stack, name, fd)
                total += allocated_bytes(child, seen)
        else:
            total += value.st_blocks * 512
    return total


def profiles(stack, entry):
    result = []
    for first in sorted(os.listdir(entry)):
        if first.startswith(PREFIX):
            continue
        info = os.stat(first, dir_fd=entry, follow_symlinks=False)
        if stat.S_ISLNK(info.st_mode):
            raise Refusal("entry contains a symlinked profile")
        if not stat.S_ISDIR(info.st_mode):
            continue
        first_fd = open_dir(stack, first, entry)
        candidates = [([first], first_fd)]
        for second in sorted(os.listdir(first_fd)):
            info = os.stat(second, dir_fd=first_fd, follow_symlinks=False)
            if stat.S_ISDIR(info.st_mode) and second != "incremental":
                candidates.append(([first, second], open_dir(stack, second, first_fd)))
        for path, profile in candidates:
            incremental = optional_stat(profile, "incremental")
            if incremental is None:
                continue
            lock = optional_stat(profile, ".cargo-lock")
            if lock is None or not stat.S_ISREG(lock.st_mode):
                raise Refusal("incremental directory lacks its Cargo profile lock")
            cache = open_dir(stack, "incremental", profile)
            result.append((path, profile, identity(profile), identity(cache), allocated_bytes(cache)))
    return result


def write_manifest(stage, manifest):
    try:
        os.unlink("manifest.new", dir_fd=stage)
    except FileNotFoundError:
        pass
    fd = os.open("manifest.new", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=stage)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(manifest, handle)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace("manifest.new", "manifest.json", src_dir_fd=stage, dst_dir_fd=stage)
    os.fsync(stage)


def read_manifest(stage, root, entry):
    fd = os.open("manifest.json", os.O_RDONLY | os.O_NOFOLLOW, dir_fd=stage)
    with os.fdopen(fd, encoding="utf-8") as handle:
        manifest = json.load(handle)
    if manifest.get("schema") != SCHEMA or manifest.get("root") != identity(root) or manifest.get("entry") != identity(entry):
        raise Refusal("staged cache ownership changed")
    if not isinstance(manifest.get("parts"), list) or not manifest["parts"]:
        raise Refusal("staged cache has no parts")
    seen = set()
    for part in manifest["parts"]:
        path = part.get("profile")
        if not isinstance(path, list) or not 1 <= len(path) <= 2:
            raise Refusal("invalid staged profile")
        for name in path:
            component(name)
        component(part["name"])
        if part["name"] in seen or not part["name"].isdigit():
            raise Refusal("duplicate or invalid staged cache part")
        seen.add(part["name"])
        if not isinstance(part.get("bytes"), int) or part["bytes"] < 0:
            raise Refusal("invalid staged cache size")
    if set(os.listdir(stage)) - seen - {"manifest.json", "manifest.new"}:
        raise Refusal("staged directory contains unowned files")
    return manifest


def profile_fd(stack, entry, part):
    profile = entry
    for name in part["profile"]:
        profile = open_dir(stack, component(name), profile)
    if identity(profile) != part["profile_identity"]:
        raise Refusal("Cargo profile identity changed")
    return profile


def inspect_part(stack, stage, part):
    if optional_stat(stage, part["name"]) is None:
        return None
    fd = open_dir(stack, part["name"], stage)
    if identity(fd) != part["identity"]:
        raise Refusal("staged incremental identity changed")
    return fd


def remaining_bytes(stack, entry, stage, manifest):
    total = 0
    for part in manifest["parts"]:
        cache = inspect_part(stack, stage, part)
        if cache is None and not part.get("retiring"):
            profile = profile_fd(stack, entry, part)
            cache = open_dir(stack, "incremental", profile)
            if identity(cache) != part["identity"]:
                raise Refusal("planned incremental cache changed before staging")
        if cache is not None:
            total += allocated_bytes(cache)
    return total


def operate(action, root_path, name, dry_run=False, stage_name=None):
    if Path(root_path).name not in ("harn-target", "release-gate-target"):
        raise Refusal("not a managed target root")
    component(name)
    if not getattr(shutil.rmtree, "avoids_symlink_attacks", False) or "dir_fd" not in inspect.signature(shutil.rmtree).parameters:
        raise Refusal("safe directory-FD removal is unavailable")
    with ExitStack() as stack:
        root = open_dir(stack, root_path)
        entry = open_dir(stack, name, root)
        if action == "stage":
            previous = [value for value in os.listdir(entry) if value.startswith(PREFIX)]
            if len(previous) > 1:
                raise Refusal("multiple staged incremental operations need recovery")
            if previous:
                stage_name = previous[0]
                stage = open_dir(stack, stage_name, entry)
                manifest = read_manifest(stage, root, entry)
            else:
                found = profiles(stack, entry)
                if not found:
                    return {"status": "empty", "parts": 0, "bytes": 0}
                parts = [{"profile": path, "profile_identity": profile_id,
                          "identity": cache_id, "bytes": size, "name": str(index)}
                         for index, (path, _, profile_id, cache_id, size) in enumerate(found)]
                manifest = {"schema": SCHEMA, "root": identity(root), "entry": identity(entry), "parts": parts}
                if dry_run:
                    return {"status": "dry-run", "parts": len(parts), "bytes": sum(p["bytes"] for p in parts)}
                stage_name = PREFIX + uuid.uuid4().hex
                os.mkdir(stage_name, 0o700, dir_fd=entry)
                stage = open_dir(stack, stage_name, entry)
                write_manifest(stage, manifest)
            if dry_run:
                return {"status": "dry-run", "stage": stage_name, "parts": len(manifest["parts"]),
                        "bytes": remaining_bytes(stack, entry, stage, manifest)}
            for part in manifest["parts"]:
                if part.get("retiring"):
                    continue
                if inspect_part(stack, stage, part) is not None:
                    continue
                profile = profile_fd(stack, entry, part)
                existing = optional_stat(profile, "incremental")
                if existing is None or [existing.st_dev, existing.st_ino] != part["identity"]:
                    raise Refusal("planned incremental cache changed before staging")
                os.rename("incremental", part["name"], src_dir_fd=profile, dst_dir_fd=stage)
                inspect_part(stack, stage, part)
            os.fsync(stage)
            return {"status": "staged", "stage": stage_name, "parts": len(manifest["parts"]), "bytes": 0}

        if stage_name is None or not component(stage_name).startswith(PREFIX):
            raise Refusal("invalid incremental stage")
        stage = open_dir(stack, stage_name, entry)
        manifest = read_manifest(stage, root, entry)
        for part in manifest["parts"]:
            inspect_part(stack, stage, part)
            profile = profile_fd(stack, entry, part)
            if action == "restore" and optional_stat(profile, "incremental") is not None:
                raise Refusal("a new incremental generation prevents restoration")
        reclaimed = 0
        try:
            for part in manifest["parts"]:
                profile = profile_fd(stack, entry, part)
                if optional_stat(stage, part["name"]) is None:
                    continue
                if action == "restore":
                    os.rename(part["name"], "incremental", src_dir_fd=stage, dst_dir_fd=profile)
                else:
                    cache = inspect_part(stack, stage, part)
                    if cache is None:
                        raise Refusal("staged incremental cache disappeared before removal")
                    size = allocated_bytes(cache)
                    part["retiring"] = True
                    write_manifest(stage, manifest)
                    shutil.rmtree(part["name"], dir_fd=stage)
                    reclaimed += size
            try:
                os.unlink("manifest.new", dir_fd=stage)
            except FileNotFoundError:
                pass
            os.unlink("manifest.json", dir_fd=stage)
            os.rmdir(stage_name, dir_fd=entry)
        except (OSError, Refusal) as error:
            # Retain a recoverable identity receipt even when only metadata
            # cleanup failed after removing some payloads.
            try:
                write_manifest(stage, manifest)
            except OSError:
                pass
            return {"status": "partial", "reason": str(error), "stage": stage_name,
                    "parts": len(manifest["parts"]), "bytes": reclaimed}
        return {"status": "restored" if action == "restore" else "reclaimed", "parts": len(manifest["parts"]), "bytes": reclaimed}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("stage", "finish", "restore"))
    parser.add_argument("root")
    parser.add_argument("entry")
    parser.add_argument("--stage")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--receipt", required=True)
    args = parser.parse_args()
    try:
        result = operate(args.action, args.root, args.entry, args.dry_run, args.stage)
    except (OSError, ValueError, TypeError, KeyError, Refusal) as error:
        result = {"status": "refused", "reason": str(error), "parts": 0, "bytes": 0}
    result.update(schema=SCHEMA, action=args.action, entry=args.entry, scope="incremental-only")
    fd = os.open(args.receipt, os.O_WRONLY | os.O_TRUNC | os.O_NOFOLLOW)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(result, handle, sort_keys=True)
        handle.write("\n")
    print(result["status"], result.get("stage", "-"), result["bytes"], result["parts"], sep="\t")


if __name__ == "__main__":
    main()
