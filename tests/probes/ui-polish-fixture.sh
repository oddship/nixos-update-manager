#!/usr/bin/env bash
set -euo pipefail
# Two real hosts prove hostname selection; an inert delay makes progress observable.
test "$(hostname)" = updates-demo
base="${1:-$HOME/ui-polish}"
demo_source="${2:-/tmp/xchg/nixos-updates-demo-source}"
bash "$demo_source/gnome-flow-fixture.sh" "$base" "$demo_source"
python3 - "$base/config/flake.nix" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
text = path.read_text().replace('outputs = { fixture, ... }:', 'outputs = { self, fixture, ... }:')
text = text.replace('  in {', '  in {\n    nixosConfigurations.updates-demo = self.nixosConfigurations.demo;')
path.write_text(text)
PY
cat > "$base/config/local.nix" <<'NIX'
{ pkgs, ... }: {
  /* working local note */
  system.extraDependencies = [ (pkgs.runCommand "ui-progress-fixture" {} ''
    sleep 90
    echo finished > $out
  '') ];
}
NIX
# The staged local note remains staged; both working edits remain uncommitted.
printf '%s\n' "$base/config"
