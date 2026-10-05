#!/usr/bin/env python3
"""Check public documentation links, media size, and obvious accidental secrets.

This is a small packaging check, not a substitute for a manual release audit.
It uses only Python's standard library and never prints matched secret values.
"""
from pathlib import Path
import re
import struct
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
failures = []
required = ["README.md", "LICENSE", "CONTRIBUTING.md", "SECURITY.md", "CHANGELOG.md",
            "docs/RELEASING.md", "docs/walkthrough/README.md", "docs/walkthrough/nixos-updates.gif"]
for name in required:
    if not (ROOT / name).is_file():
        failures.append(f"Missing public file: {name}")

# Inspect the actual publication set as well as the working tree. Ignored local
# files must not become public through an accidental force-add.
tracked = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, check=True,
                         capture_output=True).stdout.decode().split("\0")
private_parts = {".aws", ".codex", ".agents", "target", "artifacts", ".direnv", "credentials", "credentials.toml"}
for name in filter(None, tracked):
    path = Path(name)
    if (set(path.parts) & private_parts or path.suffix == ".qcow2"
            or (path.name.startswith(".env") and path.name != ".env.example")):
        failures.append(f"Private/local file is tracked: {name}")

excluded = {".git", ".aws", "target", "artifacts", ".direnv", ".codex", ".agents", "__pycache__"}
secrets = [re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----"),
           re.compile(r"gh[pousr]_[A-Za-z0-9]{30,}"),
           re.compile(r"github_pat_[A-Za-z0-9_]{50,}"),
           re.compile(r"AKIA[A-Z0-9]{16}")]
for path in ROOT.rglob("*"):
    relative = path.relative_to(ROOT)
    if set(relative.parts) & excluded or not path.is_file() or path.is_symlink():
        continue
    if path.suffix in {".png", ".gif"}:
        if path.stat().st_size > 2 * 1024 * 1024:
            failures.append(f"Media exceeds 2 MiB: {relative}")
        continue
    try:
        content = path.read_text()
    except (UnicodeError, OSError):
        continue
    if any(pattern.search(content) for pattern in secrets):
        failures.append(f"Potential secret requires review: {relative}")
    if path.suffix == ".md":
        for link in re.findall(r"!?\[[^\]]*\]\(([^)]+)\)", content):
            target = link.split('#', 1)[0]
            if not target or re.match(r"[a-zA-Z][a-zA-Z0-9+.-]*:", target):
                continue
            if not (path.parent / target).exists():
                failures.append(f"Broken link in {relative}: {target}")

animation = ROOT / "docs/walkthrough/nixos-updates.gif"
if animation.is_file():
    header = animation.read_bytes()[:10]
    if len(header) != 10 or header[:6] not in {b"GIF87a", b"GIF89a"}:
        failures.append("Walkthrough is not a GIF")
    else:
        width, height = struct.unpack("<HH", header[6:10])
        if width < 640 or height < 400:
            failures.append("Walkthrough is too small to read")

for workflow in (ROOT / ".github/workflows").glob("*.yml"):
    for dependency in re.findall(r"uses:\s*([^\s#]+)", workflow.read_text()):
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+@[0-9a-f]{40}", dependency):
            failures.append(f"Unpinned action in {workflow.name}: {dependency}")
if failures:
    print("\n".join(failures), file=sys.stderr)
    sys.exit(1)
print("Public docs, local links, walkthrough media, and action pins checked. Manual privacy review still required.")
