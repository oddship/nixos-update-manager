#!/usr/bin/env bash
set -euo pipefail
# Run only inside the project-owned disposable GNOME guest.
test "$(hostname)" = updates-demo
base="${1:-$HOME/updates-flow}"
demo_source="${2:-/tmp/xchg/nixos-updates-demo-source}"
test ! -e "$base" || { echo 'Choose a new fixture directory.' >&2; exit 1; }
for file in gnome-vm.nix module.nix; do test -f "$demo_source/$file"; done
nixpkgs_source="$(readlink -f /etc/nixos-update-manager-test/nixpkgs-source)"
test -f "$nixpkgs_source/flake.nix"
mkdir -p "$base/input" "$base/config"
for repo in input config; do
  git -C "$base/$repo" init -q
  git -C "$base/$repo" config user.name 'GNOME Fixture'
  git -C "$base/$repo" config user.email fixture@example.invalid
  git -C "$base/$repo" config commit.gpgSign false
  git -C "$base/$repo" config core.hooksPath .git/hooks
done
echo '{ outputs = { self }: { value = "old"; }; }' > "$base/input/flake.nix"
git -C "$base/input" add .
git -C "$base/input" commit -qm baseline
cp "$demo_source/gnome-vm.nix" "$base/config/guest.nix"
cp "$demo_source/module.nix" "$base/config/module.nix"
app_path="$(dirname "$(dirname "$(readlink -f /run/current-system/sw/bin/nixos-update-manager)")")"
cat > "$base/config/updater.nix" <<NIX
import ./module.nix {
  package = { type = "derivation"; name = "nixos-update-manager-0.1.0"; outPath = builtins.appendContext "$app_path" { "$app_path" = { path = true; }; }; };
}
NIX
cat > "$base/config/flake.nix" <<NIX
{
  inputs.fixture.url = "git+file://$base/input";
  outputs = { fixture, ... }: let
    # The guest already contains this exact pinned source. Avoid network access.
    nixpkgs = builtins.getFlake "path:$nixpkgs_source";
  in {
    nixosConfigurations.demo = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [ ./guest.nix ./updater.nix ./local.nix {
        services.nixos-update-manager.enable = true;
        environment.etc."updates-fixture".text = fixture.value;
      } ];
    };
  };
}
NIX
echo '{ ... }: { }' > "$base/config/local.nix"
git -C "$base/config" add .
nix --extra-experimental-features 'nix-command flakes' --max-jobs 1 --cores 2 flake lock "$base/config"
git -C "$base/config" add .
git -C "$base/config" commit -qm baseline
echo '{ ... }: { /* staged local note */ }' > "$base/config/local.nix"
git -C "$base/config" add local.nix
echo '{ ... }: { /* working local note */ }' > "$base/config/local.nix"
echo '{ outputs = { self }: { value = "new"; }; }' > "$base/input/flake.nix"
git -C "$base/input" add .
git -C "$base/input" commit -qm available-update
printf '%s\n' "$base/config"
