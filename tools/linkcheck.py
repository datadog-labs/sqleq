#!/usr/bin/env python3
# Unless explicitly stated otherwise all files in this repository are licensed under the
# Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2026-Present Datadog, Inc.

"""Every relative link in every tracked Markdown file points at a file that exists.

A dangling relative link is the one documentation defect that is both certain to happen -- files
get renamed, `docs/` gets reorganised -- and invisible to every other check in this repo, because
nothing compiles Markdown. It is also the first thing a new reader hits. So it is a gate, not a
habit: this exits non-zero and CI runs it.

Two deliberate choices, both of which have already mattered here:

* The file list comes from `git ls-files --cached --others --exclude-standard`, not from `--cached`
  alone. A doc that has been written but not yet `git add`ed is exactly the doc whose links have
  never been checked, and with `--cached` only, this script passes over it in silence.
* HTML `src` / `srcset` / `href` attributes are checked alongside Markdown `[](...)`. The README's
  title line is a `<picture>` element holding the logo; moving `docs/logo/` would break the very
  first line of the repo's front page without touching a single Markdown link.

Fragments (`#section`) are dropped rather than resolved. Checking them would mean deciding what
GitHub's heading slugs are, which is a guess about someone else's renderer; checking the path is
not.
"""

import os
import re
import subprocess
import sys

# `[text](target)`, allowing the optional `(target "title")` form. `target` runs to the first
# whitespace or `)`, which is all this repo's links need -- no `<angle-bracket>` or escaped-paren
# targets exist here, and a link that grew one would show up as a dangling path rather than pass.
MD_LINK = re.compile(r"\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
HTML_ATTR = re.compile(r"(?:src|srcset|href)=\"([^\"]+)\"")
FENCE = re.compile(r"^```", re.M)
EXTERNAL = ("http://", "https://", "mailto:", "//", "#")


def strip_fences(src: str) -> str:
    """Blank out fenced code blocks, keeping line numbers intact so reports stay accurate."""
    out, fenced = [], False
    for line in src.split("\n"):
        if FENCE.match(line):
            fenced = not fenced
            out.append("")
        else:
            out.append("" if fenced else line)
    return "\n".join(out)


def targets(src: str):
    """Yield `(line number, target)` for every link worth resolving."""
    for lineno, line in enumerate(strip_fences(src).split("\n"), 1):
        for pat in (MD_LINK, HTML_ATTR):
            for m in pat.finditer(line):
                t = m.group(1)
                if not t.startswith(EXTERNAL):
                    yield lineno, t


def main() -> int:
    root = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True
    ).stdout.strip()
    os.chdir(root)
    listing = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split("\n")
    files = [f for f in listing if f.endswith(".md")]

    bad = []
    for f in files:
        with open(f, encoding="utf-8") as fh:
            src = fh.read()
        for lineno, t in targets(src):
            path = t.partition("#")[0]
            if not path:
                continue
            resolved = os.path.normpath(os.path.join(os.path.dirname(f), path))
            if not os.path.exists(resolved):
                bad.append((f, lineno, t))

    for f, lineno, t in bad:
        print(f"{f}:{lineno}: dangling link -> {t}")
    print(f"{len(files)} markdown files checked, {len(bad)} dangling link(s)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
