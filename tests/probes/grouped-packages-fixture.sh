#!/usr/bin/env bash
set -euo pipefail
# Run only inside the disposable GNOME guest, never a real host configuration.
base="${1:-$HOME/grouped-packages}"
demo_source="${2:-/tmp/xchg/grouped-source}"
bash "$demo_source/gnome-flow-fixture.sh" "$base" "$demo_source"
nixpkgs_source="$(readlink -f /etc/nixos-update-manager-test/nixpkgs-source)"
write_input() {
  cat > "$base/input/flake.nix" <<NIX
{ outputs = { self }: let
  pkgs = import $nixpkgs_source { system = "x86_64-linux"; };
  package = n: pkgs.runCommand "group-demo\${toString n}-$1" {} "mkdir -p \$out/share/group-demo\${toString n}; echo $1 > \$out/share/group-demo\${toString n}/version";
in {
  value = "$2";
  packages.x86_64-linux = builtins.listToAttrs (builtins.genList (n: { name = "demo\${toString n}"; value = package n; }) 10);
}; }
NIX
  git -C "$base/input" add flake.nix
  git -C "$base/input" commit -qm "ten packages at $1"
}
write_input 1.0.0 old
python3 - "$base/config/flake.nix" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
s = p.read_text().replace('outputs = { fixture, ... }:', 'outputs = { self, fixture, ... }:')
s = s.replace('  in {', '  in {\n    nixosConfigurations.updates-demo = self.nixosConfigurations.demo;')
s = s.replace('modules = [ ./guest.nix', 'modules = [ { environment.systemPackages = builtins.attrValues fixture.packages.x86_64-linux; } ./guest.nix')
p.write_text(s)
PY
nix --extra-experimental-features 'nix-command flakes' --max-jobs 1 --cores 2 flake update fixture --flake "$base/config"
write_input 1.1.0 new
printf '%s\n' "$base/config"
