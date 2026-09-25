#!/usr/bin/env python3
"""Fail if any include_str! or include_bytes! path is missing on disk.

Paths are resolved relative to the Rust source file, matching rustc.
A macro whose argument is not a string literal is also a failure, so a
concat! or env! include cannot slip past this check.
"""

import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SKIP_DIRS = {".git", "target", "dist"}


def skip_ws(text, i):
    while i < len(text) and text[i].isspace():
        i += 1
    return i


def parse_string(text, i):
    hashes = 0
    if text.startswith("r", i):
        j = i + 1
        while j < len(text) and text[j] == "#":
            hashes += 1
            j += 1
        if j >= len(text) or text[j] != '"':
            return None
        j += 1
        closer = '"' + ("#" * hashes)
        end = text.find(closer, j)
        if end < 0:
            return None
        return text[j:end], end + len(closer)
    if i >= len(text) or text[i] != '"':
        return None
    j = i + 1
    chars = []
    while j < len(text):
        ch = text[j]
        if ch == "\\":
            if j + 1 >= len(text):
                return None
            esc = text[j + 1]
            mapping = {"n": "\n", "r": "\r", "t": "\t", "\\": "\\", '"': '"', "0": "\0"}
            if esc not in mapping:
                return None
            chars.append(mapping[esc])
            j += 2
            continue
        if ch == '"':
            return "".join(chars), j + 1
        chars.append(ch)
        j += 1
    return None


def skip_string(text, i):
    """Move past a string or raw string that starts at i. Return None if it is not one."""
    parsed = parse_string(text, i)
    if parsed is None:
        return None
    return parsed[1]


def includes_in(text):
    """Macros in code. Names that appear inside strings or comments are ignored."""
    found = []
    i = 0
    n = len(text)
    while i < n:
        if text.startswith("//", i):
            newline = text.find("\n", i)
            i = n if newline < 0 else newline + 1
            continue
        if text.startswith("/*", i):
            end = text.find("*/", i + 2)
            if end < 0:
                raise SystemExit("unterminated block comment")
            i = end + 2
            continue
        skipped = skip_string(text, i)
        if skipped is not None:
            i = skipped
            continue
        kind = None
        if text.startswith("include_str!", i):
            kind = "include_str!"
        elif text.startswith("include_bytes!", i):
            kind = "include_bytes!"
        if kind is None:
            i += 1
            continue
        cursor = skip_ws(text, i + len(kind))
        if cursor >= n or text[cursor] != "(":
            found.append((kind, None))
            i += len(kind)
            continue
        parsed = parse_string(text, skip_ws(text, cursor + 1))
        if parsed is None:
            found.append((kind, None))
            i += len(kind)
            continue
        path, end = parsed
        close = skip_ws(text, end)
        if close >= n or text[close] != ")":
            found.append((kind, None))
        else:
            found.append((kind, path))
        i = close + 1 if close < n else end
    return found


def rust_files():
    for dirpath, dirnames, filenames in os.walk(ROOT):
        dirnames[:] = [name for name in dirnames if name not in SKIP_DIRS]
        for name in filenames:
            if name.endswith(".rs"):
                yield os.path.join(dirpath, name)


def main():
    missing = []
    checked = 0
    for path in sorted(rust_files()):
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        for kind, include in includes_in(text):
            rel = os.path.relpath(path, ROOT)
            if include is None:
                missing.append(f"{rel}: {kind} argument is not a string literal")
                continue
            target = os.path.normpath(os.path.join(os.path.dirname(path), include))
            checked += 1
            if not os.path.isfile(target):
                missing.append(f"{rel}: {kind}({include}) -> {os.path.relpath(target, ROOT)}")
    if missing:
        print("embedded include paths that are not in the tree:", file=sys.stderr)
        for line in missing:
            print(f"  {line}", file=sys.stderr)
        return 1
    print(f"checked {checked} include_str!/include_bytes! paths")
    return 0


if __name__ == "__main__":
    sys.exit(main())
