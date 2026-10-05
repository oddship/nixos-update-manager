#!/usr/bin/env python3
"""Exercise real Nix against synthetic Git flakes. Never activates a system."""
import json
import os
from pathlib import Path
import subprocess
import tempfile


def run(args, cwd=None):
    p = subprocess.run(args, cwd=cwd, text=True, capture_output=True, check=True)
    return p.stdout.strip()


def git(repo, *args):
    return run(["git", "-C", str(repo), *args])


def init(repo):
    repo.mkdir()
    git(repo, "init", "-q")
    git(repo, "config", "user.name", "Fixture")
    git(repo, "config", "user.email", "fixture@example.invalid")
    git(repo, "config", "commit.gpgSign", "false")
    git(repo, "config", "core.hooksPath", ".git/hooks")


def commit(repo):
    git(repo, "add", ".")
    git(repo, "commit", "-qm", "fixture")


def nix(*args):
    return run(["nix", "--extra-experimental-features", "nix-command flakes", *map(str, args)])


with tempfile.TemporaryDirectory(prefix="nixos-updates-probe-") as tmp:
    root = Path(tmp)
    dep, repo = root / "dep", root / "repo"
    init(dep)
    (dep / "flake.nix").write_text('{ outputs = { self }: { value = "one"; }; }')
    commit(dep)
    init(repo)
    flake = repo / "config"
    flake.mkdir()
    (repo / "shared.nix").write_text('"tracked"')
    (repo / ".gitignore").write_text("private\n")
    (repo / "private").write_text("must-not-be-imported")
    (flake / "flake.nix").write_text('''{
      inputs.dep.url = "git+file://''' + str(dep) + '''";
      outputs = { self, dep }: {
        nixosConfigurations = {
          good.config.system.build.toplevel = derivation {
            name = "probe-" + dep.value + "-" + (import ../shared.nix);
            system = "x86_64-linux";
            builder = "/does-not-exist";
          };
          broken = throw "unrelated host must not be evaluated";
        };
        probe = {
          value = dep.value;
          local = import ../shared.nix;
          filtered = !(builtins.pathExists (self.outPath + "/../private"));
          lock = builtins.fromJSON (builtins.readFile (self.outPath + "/flake.lock"));
          metadata = self.sourceInfo;
        };
      };
    }''')
    git(repo, "add", ".")
    nix("flake", "lock", flake)
    commit(repo)
    original = (flake / "flake.lock").read_bytes()
    (repo / "shared.nix").write_text('"dirty-tracked"')
    # An unrelated staged file must survive byte-for-byte in the real index.
    (repo / "staged").write_text("staged content")
    git(repo, "add", "staged")
    index = (repo / ".git/index").read_bytes()
    before = git(repo, "status", "--porcelain=v1")
    (dep / "flake.nix").write_text('{ outputs = { self }: { value = "two"; }; }')
    commit(dep)
    candidate = root / "candidate.lock"
    nix("flake", "update", "dep", "--flake", flake, "--output-lock-file", candidate)
    common = ["--no-write-lock-file", "--no-update-lock-file", "--option", "allow-import-from-derivation", "false"]
    hosts = json.loads(nix("eval", "--json", str(flake) + "#nixosConfigurations", "--apply", "builtins.attrNames", *common))
    old = nix("eval", "--raw", str(flake) + "#nixosConfigurations.good.config.system.build.toplevel.drvPath", *common)
    new = nix("eval", "--raw", str(flake) + "#nixosConfigurations.good.config.system.build.toplevel.drvPath", "--reference-lock-file", candidate, *common)
    probe = json.loads(nix("eval", "--json", str(flake) + "#probe", "--reference-lock-file", candidate, *common))
    assert hosts == ["broken", "good"]
    assert old != new
    assert probe["value"] == "two" and probe["local"] == "dirty-tracked"
    assert (flake / "flake.lock").read_bytes() == original
    assert (repo / ".git/index").read_bytes() == index
    assert git(repo, "status", "--porcelain=v1") == before
    lock_visible = probe["lock"] == json.loads(candidate.read_text())
    binary = Path(__file__).resolve().parents[2] / "target/debug/nixos-update-manager"
    if binary.exists():
        snapshot = root / "snapshot"
        run([str(binary), "snapshot", str(flake), str(snapshot)])
        assert not (snapshot / "private").exists()
        assert (snapshot / "shared.nix").read_text() == '"dirty-tracked"'
        (snapshot / "config/flake.lock").write_bytes(candidate.read_bytes())
        copied = json.loads(nix("eval", "--json", str(snapshot / "config") + "#probe", *common))
        assert copied["lock"] == json.loads(candidate.read_text())
        assert copied["value"] == "two"
        state_dir = root / "state"
        checked = json.loads(run([str(binary), "--state-dir", str(state_dir), "check", str(flake), "--host", "good"]))
        assert checked["state"] == "updates_found", checked
        assert checked["changed_inputs"] == ["dep"]
        run([str(binary), "--state-dir", str(state_dir), "fresh", checked["id"]])
        (repo / "shared.nix").write_text('"concurrent-edit"')
        stale = subprocess.run([str(binary), "--state-dir", str(state_dir), "fresh", checked["id"]], capture_output=True)
        assert stale.returncode != 0
        (repo / "shared.nix").write_text('"dirty-tracked"')
        assert (flake / "flake.lock").read_bytes() == original
        assert (repo / ".git/index").read_bytes() == index
        assert git(repo, "status", "--porcelain=v1") == before
    print(json.dumps({"nix": nix("--version"), "hosts": hosts, "old_drv": old, "new_drv": new,
        "checkout_and_index_preserved": True, "candidate_lock_visible_through_self": lock_visible,
        "snapshot_and_backend_verified": binary.exists(),
        "metadata": probe["metadata"]}, indent=2))
