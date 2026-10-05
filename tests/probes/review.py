#!/usr/bin/env python3
"""Read-only real-Nix review proof; never build or activate a system."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

binary = Path(__file__).resolve().parents[2] / "target/debug/nixos-update-manager"
running = Path("/run/current-system").resolve(strict=True)
real_nix = shutil.which("nix")
assert real_nix and binary.is_file(), "build the backend first"

def run(args, **kwargs):
    return subprocess.check_output([str(a) for a in args], text=True, **kwargs)

with tempfile.TemporaryDirectory(prefix="updates-review-") as tmp:
    root = Path(tmp)
    repo, state, shim_dir = root / "repo", root / "state", root / "bin"
    repo.mkdir()
    shim_dir.mkdir()
    run(["git", "-C", repo, "init", "-q"])
    run(["git", "-C", repo, "config", "user.name", "Fixture"])
    run(["git", "-C", repo, "config", "user.email", "fixture@example.invalid"])
    run(["git", "-C", repo, "config", "commit.gpgSign", "false"])
    run(["git", "-C", repo, "config", "core.hooksPath", ".git/hooks"])
    (repo / "flake.nix").write_text('{ outputs = { self }: { nixosConfigurations.fixture = {}; }; }')
    (repo / "flake.lock").write_text(json.dumps({"version": 7, "root": "root", "nodes": {"root": {"inputs": {}}}}))
    run(["git", "-C", repo, "add", "."])
    run(["git", "-C", repo, "commit", "-qm", "fixture"])
    command = [binary, "--state-dir", state]
    checked = json.loads(run([*command, "check", repo, "--host", "fixture"]))
    assert checked["state"] == "up_to_date", checked
    directory = state / checked["id"]
    # Seed an already-realised output; refresh must never realise anything.
    (directory / "gc-root").symlink_to(running)
    checked.update(state="ready", prepared_system=str(running), review_baseline="/stale-fixture-baseline", closure_diff="obsolete")
    (directory / "state.json").write_text(json.dumps(checked))
    shim = shim_dir / "nix"
    shim.write_text('#!/usr/bin/env python3\nimport json,os,sys\nwith open(os.environ["REVIEW_ARGS"],"a") as f: f.write(json.dumps(sys.argv[1:])+"\\n")\nos.execv(os.environ["REVIEW_NIX"],[os.environ["REVIEW_NIX"],*sys.argv[1:]])\n')
    shim.chmod(0o755)
    log = root / "commands.jsonl"
    env = {**os.environ, "PATH": str(shim_dir) + ":" + os.environ["PATH"], "REVIEW_NIX": real_nix, "REVIEW_ARGS": str(log)}
    reviewed = json.loads(run([*command, "review", checked["id"]], env=env))
    assert reviewed["state"] == "ready", reviewed
    assert reviewed["prepared_system"] == reviewed["review_baseline"] == str(running)
    calls = [json.loads(line) for line in log.read_text().splitlines()]
    assert len(calls) == 1 and calls[0][calls[0].index("store"):][:2] == ["store", "diff-closures"], calls
    assert not reviewed["closure_diff"].strip(), reviewed["closure_diff"]
    assert "\x1b" not in reviewed["closure_diff"]
    assert json.loads(run([*command, "history"]))[0]["id"] == checked["id"]
    run([*command, "fresh", checked["id"]])
    print(json.dumps({"real_nix_review": True, "builds_invoked": 0, "baseline_refreshed": True, "checkout_clean": not run(["git", "-C", repo, "status", "--porcelain"]).strip()}, indent=2))
