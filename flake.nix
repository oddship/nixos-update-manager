{
  description = "NixOS Update Manager: isolated flake update review";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  # Normal flake commands use these defaults when flake config is trusted.
  # The justfile/scripts pass explicit limits without requiring that trust.
  nixConfig = { cores = 2; max-jobs = 1; };
  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      # Documentation/media changes should not trigger a fresh Rust compilation.
      rustSource = pkgs.lib.cleanSourceWith {
        name = "source";
        src = self;
        filter = path: type:
          !(builtins.elem (baseNameOf path) [ ".aws" "__pycache__" "docs" "scripts" "nix" "extension" ".github" "README.md" "SPEC.md" "CONTRIBUTING.md" "SECURITY.md" "CHANGELOG.md" ".gitignore" "flake.nix" "flake.lock" "justfile" "indicator.js" ])
          && (type == "directory" || !(pkgs.lib.hasInfix "/tests/probes/" path));
      };
      indicator = pkgs.stdenvNoCC.mkDerivation {
        pname = "nixos-update-manager-gnome-extension";
        version = "0.1.0";
        src = ./extension;
        dontBuild = true;
        installPhase = ''
          directory=$out/share/gnome-shell/extensions/nixos-updates@oddship.github.io
          mkdir -p "$directory"
          cp extension.js model.js metadata.json stylesheet.css "$directory/"
        '';
        passthru.extensionUuid = "nixos-updates@oddship.github.io";
        meta = {
          description = "GNOME top-bar status for NixOS Update Manager";
          license = pkgs.lib.licenses.mit;
          platforms = [ system ];
        };
      };
      # nixConfig can be declined by callers. Bound Rust jobs inside the
      # derivation too, while honoring a smaller --cores setting.
      limitRustJobs = ''
        if [ "$NIX_BUILD_CORES" -eq 0 ] || [ "$NIX_BUILD_CORES" -gt 2 ]; then
          export NIX_BUILD_CORES=2
        fi
      '';
      package = pkgs.rustPlatform.buildRustPackage {
        pname = "nixos-update-manager";
        version = "0.1.0";
        src = rustSource;
        cargoLock.lockFile = ./Cargo.lock;
        preBuild = limitRustJobs + ''
          # The app and helper share one output; embed its trusted helper path.
          export NIXOS_UPDATES_HELPER="$out/bin/nixos-update-manager-helper"
        '';
        env.NIXOS_UPDATES_NIX_ENV = "${pkgs.nix}/bin/nix-env";
        cargoBuildFlags = [ "--bins" ];
        nativeBuildInputs = [ pkgs.makeWrapper pkgs.pkg-config pkgs.wrapGAppsHook4 ];
        buildInputs = [ pkgs.gtk4 pkgs.libadwaita ];
        nativeCheckInputs = [ pkgs.git pkgs.python3 ];
        # Only the desktop app needs a GTK/Pi wrapper. Keep the authenticated
        # helper as a direct ELF executable for the exact polkit policy path.
        dontWrapGApps = true;
        postInstall = ''
          ln -s nixos-update-manager-helper $out/bin/nixos-updates-helper
          ln -s nixos-update-manager $out/bin/nixos-updates
          install -Dm644 data/io.github.oddship.NixOSUpdates.desktop $out/share/applications/io.github.oddship.NixOSUpdates.desktop
          mkdir -p $out/share/dbus-1/services
          cat > $out/share/dbus-1/services/io.github.oddship.NixOSUpdates.service <<EOF
          [D-BUS Service]
          Name=io.github.oddship.NixOSUpdates
          Exec=$out/bin/nixos-update-manager
          EOF
        '';
        meta = {
          description = "Native GNOME update review for local NixOS flake repositories";
          homepage = "https://github.com/oddship/nixos-update-manager";
          license = pkgs.lib.licenses.mit;
          mainProgram = "nixos-update-manager";
          platforms = [ system ];
        };
        postFixup = ''
          wrapGApp $out/bin/nixos-update-manager --set NIXOS_UPDATES_PI ${pkgs.pi-coding-agent}/bin/pi --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.nix pkgs.git pkgs.systemd pkgs.gnome-console ]}
        '';
      };
    in {
      packages.${system} = {
        default = package;
        gnome-extension = indicator;
      };
      apps.${system}.default = {
        type = "app";
        program = "${package}/bin/nixos-update-manager";
      };
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [ just cargo rustc rustfmt clippy pkg-config gtk4 libadwaita git qemu python3 imagemagick fontconfig gjs pi-coding-agent ];
        shellHook = ''
          export CARGO_BUILD_JOBS="''${CARGO_BUILD_JOBS:-2}"
          export RUST_TEST_THREADS="''${RUST_TEST_THREADS:-$CARGO_BUILD_JOBS}"
          export NIX_CONFIG="''${NIX_CONFIG:-}
          max-jobs = 1
          cores = $CARGO_BUILD_JOBS"
        '';
      };
      nixosConfigurations.demo = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          ./nix/gnome-vm.nix
          (import ./nix/module.nix { inherit package indicator; })
          { services.nixos-update-manager.enable = true; }
        ];
      };
      checks.${system} = {
        package-interfaces = pkgs.runCommand "nixos-update-manager-package-interfaces" { } ''
          ${package}/bin/nixos-update-manager --help > default-help
          grep -q apply-plan default-help
          ${package}/bin/nixos-update-manager-helper --help > helper-help
          grep -q expected-running helper-help
          test "$(readlink -f ${package}/bin/nixos-updates-helper)" = "${package}/bin/nixos-update-manager-helper"
          test "$(od -An -tx1 -N4 ${package}/bin/nixos-update-manager-helper | tr -d ' ')" = 7f454c46
          test ! -e ${package}/bin/.nixos-update-manager-helper-wrapped
          ${package}/bin/nixos-updates --version > legacy-version
          ${package}/bin/nixos-update-manager --version > canonical-version
          cmp legacy-version canonical-version
          touch $out
        '';
        vm-smoke = import ./nix/vm-smoke.nix { inherit pkgs; };
        vm-helper = import ./nix/vm-helper.nix {
          inherit pkgs package;
          module = import ./nix/module.nix { inherit package; };
        };
        vm-apply = import ./nix/vm-apply.nix {
          inherit pkgs package;
          module = import ./nix/module.nix { inherit package; };
        };
      };
      nixosModules.default = import ./nix/module.nix { inherit package indicator; };
    };
}
