#!/usr/bin/env python3
"""Extract every skilj-core error variant, with its wire code and message.

Usage: scripts/extract-skilj-errors.py <path to skilj checkout> > knowledge/skilj-errors.json

Reads the `pub enum Error` of each skilj-core module and its
`SkiljRejection::code()` impl. Run it again on a new skilj version;
causes and fixes live separately in knowledge/error-fixes.toml.
"""
import json
import os
import re
import subprocess
import sys

MODULES = {
    "access_control": "access_control/mod.rs",
    "event_store": "event_store/mod.rs",
    "projections": "projections/mod.rs",
    "bootstrap": "bootstrap/mod.rs",
    "encryption": "encryption/mod.rs",
    "core": "error.rs",
}
# Variants of skilj_core::Error that only wrap another module's errors, or
# carry a code chosen elsewhere (a command's own rejection kind).
PASS_THROUGH = {"AccessControl", "EventStore", "Projections", "Bootstrap", "Encryption",
                "CommandRejected", "BatchFailed"}


def rust_strings(text):
    """The string literals in an #[error(...)], joined, with Rust's
    backslash-newline continuations and escapes resolved."""
    parts = re.findall(r'"((?:[^"\\]|\\.)*)"', text, re.S)
    joined = " ".join(parts)
    joined = re.sub(r"\\\n\s*", "", joined)
    return joined.replace('\\"', '"').replace("\\\\", "\\")


def extract(root):
    errors = []
    for module, rel in MODULES.items():
        src = open(os.path.join(root, "skilj-core/src", rel)).read()
        enum = re.search(r"pub enum Error \{(.*?)\n\}", src, re.S)
        if not enum:
            continue
        codes = {}
        impl = re.search(r"impl [\w:]*SkiljRejection for [\w:]*Error \{(.*?)\n\}", src, re.S)
        if impl:
            for arm in re.finditer(
                r'(?:Error|Self)::([A-Z][A-Za-z0-9]*)\b[^=]*=>\s*\{?\s*"([a-z0-9_]+)"', impl.group(1)
            ):
                codes.setdefault(arm.group(1), arm.group(2))
        for v in re.finditer(
            r"#\[error\((.*?)\)\]\s*(?:#\[[^\]]*\]\s*)*([A-Z][A-Za-z0-9]*)", enum.group(1), re.S
        ):
            variant = v.group(2)
            if variant in PASS_THROUGH:
                continue
            if variant not in codes:
                sys.exit(f"{module}::{variant} has no code() arm; update this script")
            errors.append({
                "module": module,
                "variant": variant,
                "code": codes[variant],
                "message": rust_strings(v.group(1)),
            })
    return errors


def main():
    root = sys.argv[1]
    version = subprocess.run(
        ["git", "-C", root, "describe", "--tags"], capture_output=True, text=True
    ).stdout.strip().lstrip("v")
    json.dump({"skilj_version": version, "errors": extract(root)}, sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    main()
