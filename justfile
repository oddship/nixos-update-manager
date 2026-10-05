set shell := ["bash", "-euo", "pipefail", "-c"]

jobs := env("CARGO_BUILD_JOBS", "2")
export CARGO_BUILD_JOBS := jobs

# List development commands.
default:
    @just --list

# Enter the pinned development shell (also works before files are tracked).
dev:
    bash scripts/nix-dev

# Build the native development preview, at lower CPU scheduling priority.
build:
    bash scripts/nix-dev -c nice -n 10 cargo build --locked

# Incremental local launches avoid a fresh packaged release build after edits.
[positional-arguments]
run *args:
    bash scripts/nix-dev -c nice -n 10 cargo run --locked -- "$@"

test:
    bash scripts/nix-dev -c nice -n 10 cargo test --locked

lint:
    bash scripts/nix-dev -c cargo fmt --check
    bash scripts/nix-dev -c nice -n 10 cargo clippy --locked --all-targets -- -D warnings

fmt:
    bash scripts/nix-dev -c cargo fmt

# Build the wrapped release package, one Nix derivation at a time.
package:
    bash scripts/nix-build packages.x86_64-linux.default --out-link artifacts/package

vm-test:
    bash scripts/nix-build checks.x86_64-linux.vm-smoke --out-link artifacts/vm-smoke

helper-test:
    bash scripts/nix-build checks.x86_64-linux.vm-helper --out-link artifacts/vm-helper

apply-test:
    bash scripts/nix-build checks.x86_64-linux.vm-apply --out-link artifacts/vm-apply

vm:
    bash scripts/gnome-vm

# Run synthetic Nix probes after building the app.
probes: build
    bash scripts/nix-dev -c python3 tests/probes/alternate_lock.py
    bash scripts/nix-dev -c python3 tests/probes/review.py
    bash scripts/nix-dev -c python3 tests/probes/input-packages.py
# Check/prepare a real synthetic NixOS flake without activating it.
real-prepare-probe: build
    mkdir -p artifacts
    bash scripts/nix-dev -c nice -n 10 python3 tests/probes/full_nixos.py --output artifacts/full-nixos.json

# Test the GNOME indicator's state model.
indicator-test:
    bash scripts/nix-dev -c gjs -m tests/indicator.js

# Verify public documentation, local links, media size, and action pins.
public-check:
    python3 scripts/check-public-repo.py

# Re-encode retained genuine GNOME guest captures.
walkthrough:
    bash scripts/nix-dev -c nice -n 10 bash scripts/walkthrough-gif
