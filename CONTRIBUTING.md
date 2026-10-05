# Contributing

Start with an issue for changes to activation, Git transactions, or assistant authority. Focused fixes are welcome. Report vulnerabilities privately through [SECURITY.md](SECURITY.md).

## Development

```sh
nix develop
just run
just lint
just test
just indicator-test
```

`just run` uses the incremental Cargo build. `just build`, `just test`, and `just lint` check the same app; `just package` builds its single wrapped Nix package, including Pi and the privileged helper. `just --list` shows the remaining commands. Add new source files to Git before testing a Git-backed flake, since Nix only includes tracked files.

Defaults allow two compiler jobs/test threads and one Nix derivation with two build cores, at lower scheduling priority. Use `just jobs=1 build` on a smaller machine. App-issued Nix commands also default to one derivation/two cores; a positive `NIXOS_UPDATES_BUILD_CORES` overrides cores and is forwarded to GUI jobs. These limits are not a CPU quota.

## Architecture

The GUI/CLI share the Rust backend. Private candidate snapshots, operation workers, the root helper, and Git transactions have separate authority. The GNOME indicator only reads state and opens the app. See [the architecture contract](SPEC.md) before changing these boundaries.

Preserve atomic records, freshness checks, exact prepared-system activation, truthful partial failure states, and lock-only commit attribution. Do not add live-checkout evaluation during Apply, automatic staging/pushing, or implicit assistant credential access.

## Validation

Use synthetic repositories and scripted Pi responses. Automated checks must not read a contributor's real configuration, signing keys, credentials, or provider account, or make cloud requests.

`just vm-test`, `just helper-test`, and `just apply-test` use disposable headless guests. `just vm` boots a separate GNOME guest with a project-owned disk and private QMP monitor. The host Nix store is shared read-only; no host home or configuration is mounted. Stop the guest and move its disposable disk aside for a clean run. Never switch the developer's host system to prove a fix.

`just probes` runs the alternate-lock, retained-review, and input-export probes. `just real-prepare-probe` checks/prepares a synthetic NixOS system without activation and writes its result under `artifacts/`.

For manual review in the GNOME guest, provide a fixture-source directory containing `nix/gnome-vm.nix` and `nix/module.nix` as `gnome-vm.nix` and `module.nix`, plus `tests/probes/gnome-flow-fixture.sh` and `tests/probes/grouped-packages-fixture.sh`. Then run inside the guest:

```sh
bash /path/to/fixture-source/grouped-packages-fixture.sh "$HOME/grouped-demo" /path/to/fixture-source
```

Use a new fixture directory for each run. Select its printed configuration path in the app, then Check and Prepare. The fixture uses the guest's pinned source at `/etc/nixos-update-manager-test/nixpkgs-source`. Inspect progress, groups, notifications, and Settings; use scripted Pi responses for model checks. Do not substitute a real configuration or provider account.

For activation/Git changes, verify actual running-system/profile paths, exact commit contents, and preserved unrelated edits. Include relevant interruption, authentication, hook, and signing failures. Use the guest for visible UI, notification, and indicator changes; compilation alone is not graphical validation.

Describe checks and remaining gaps in the pull request. See [status](docs/STATUS.md) and [release criteria](docs/RELEASING.md).
