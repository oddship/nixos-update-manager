# Usage and recovery

NixOS Update Manager works with a local Git repository with at least one commit, tracked `flake.nix` and `flake.lock` files and a host under `nixosConfigurations`. The flake may live in a subdirectory, such as one flake per host (`hosts/<name>/flake.nix`) sharing modules through a relative input like `path:../../common`. Choose the folder containing `hosts/`: each host is listed from its directory name and checked, applied, and committed against its own `flake.nix` and `flake.lock`. Per-host flakes take precedence over a `flake.nix` in the chosen folder. See the [README](../README.md) for installation and [status](STATUS.md) for the development preview's tested scope.

## Check, prepare, and review

Choose a configuration folder and host. Host selection prefers a saved choice, exact hostname, short hostname, then a sole discovered host. Several unmatched hosts require an explicit choice.

**Check for updates** resolves inputs in a private Git-filtered snapshot. Tracked local edits are included; ignored and untracked contents are excluded. Checking can fetch metadata/source trees, but does not intentionally build the system. Check and Prepare preserve the checkout and index.

**Prepare update** builds the recorded candidate and retains it with GC roots. Check/Prepare run as systemd user jobs, so closing the window does not stop them. A completion notification reopens the review; the app also shows recorded status when notifications are disabled.

After preparation, review shows old/new package version sets, additions, removals, unversioned entries, and size changes. Input repository revisions are separate from application versions. Technical details and Raw Nix review remain collapsed until needed.

Package groups use explicitly exported `input.packages.<system>` entries present in the candidate closure. Check evaluates at most 512 exports per input without building them. Grouping requires exact name/new-version matching and a unique input; aliases are deduplicated. Shared, removed, overlay-only, and unmapped changes stay in Other packages and system components. A closure diff does not predict every service restart or security consequence.

## Apply and optional commit

Authenticated Apply requires the NixOS module with `services.nixos-update-manager.enable = true`. It installs the app, root helper, polkit policy, and required pkexec integration. Home Manager is not required.

**Apply update…** checks freshness and the runtime baseline, confirms the operation, then requests administrator authentication. It activates the exact retained system without evaluating or rebuilding the live checkout. Only the reviewed lock bytes are written after authentication.

An optional commit includes only the reviewed lock change, preserves unrelated staged/working changes, and honors configured hooks and signing. Automatic commits require the original lock to match HEAD and the index. Included tracked source edits remain uncommitted: a lock-only commit is not a complete record of the built system.

If Git fails after activation, the system remains applied. **Retry commit** retries Git without reapplying. The app never pushes automatically.

## Recovery and state

Source, lock, index, or HEAD changes can invalidate a candidate. Recheck rather than applying an outdated candidate. When the running system changes, **Refresh review** compares the retained output against the new baseline without rebuilding, after checking source freshness.

**Cancel** stops a checking/preparing worker's command process group. A late cancellation cannot cancel a later retry. Cancelling a Nix client does not guarantee that a shared daemon build immediately stops. Failed/cancelled preparations with a retained derivation can be retried after freshness checks.

Reopening reconciles abandoned checking/preparing records under the operation lock. Recheck an interrupted check; retry an interrupted preparation. Keep state and recovery records when Apply reports uncertainty. Matching running-system/profile links alone do not prove successful activation: recorded helper completion is also required. Partial activation does not imply rollback.

Interrupted commits are reconciled only when the exact commit, source, lock, and index match the durable intent. The backend can repair an index after verified ref publication or permit commit-only retry for an unpublished intent. External changes and foreign Git locks are preserved. Unknown states require attention and may need manual HEAD/index reconciliation.

State is private under `$XDG_STATE_HOME/nixos-updates`, or `~/.local/state/nixos-updates`. Keep it outside the configuration repository. There is no retention UI; deleting an abandoned candidate directory removes its indirect GC-root links. Do not remove active or unresolved transaction records.

## Optional indicator

Set `services.nixos-update-manager.indicator.enable = true`, rebuild, and log back in. Enable the GNOME 50 extension yourself through GNOME Extensions or:

```sh
gnome-extensions enable nixos-updates@oddship.github.io
# To disable it:
gnome-extensions disable nixos-updates@oddship.github.io
```

The indicator reads default state for the last checked/saved folder and host; custom `--state-dir` locations are unsupported. It only displays recorded status and opens the app. It never runs Nix, Git, or Apply. Reads are bounded; histories beyond 256 records show neutral Open app status.

## Update assistant

Pi 1.0 is included in the default package. In **Settings**, click **Test connection** to use your existing Pi account/model. This sends a fixed short model request only on a click, with no update data and a 60-second deadline. Opening Settings sends no request. **Account and model** holds an optional override and **Open Pi** for `/login` and `/model` in GNOME Console; provider keys are not copied into app state.

A successful test enables **Explain this update** beside prepared actions. Explanation sends the phase, input names, and at most 200 package changes to the selected model provider. Files, paths, hashes, and error logs are excluded. The result has no supplied release notes and cannot establish security fixes or activation safety.

Verification is saved atomically for the executable/model. Editing the override invalidates it; delayed old tests cannot re-enable a changed model. Explanation failure also clears verification. Saved status means the last test passed, not live health: external Pi account/model files are not monitored, so retest after changing them.

Explanation runs from a neutral directory with tools, extensions, skills, prompt templates, context files, session persistence, and automatic approval disabled. Output is bounded, with a 120-second deadline and process-group cleanup. These options are not an OS sandbox. Chat, editing, repair, build, Apply, and commit are not assistant capabilities. Native assistant checks use synthetic responses; real provider integration remains unverified.

## CLI

The app includes CLI commands using the same backend as its GUI and emits schema-versioned JSON. No separate CLI package or build is needed. Replace the example path, host, and candidate ID:

```sh
nixos-update-manager inspect /path/to/config
nixos-update-manager check /path/to/config --host laptop
nixos-update-manager show candidate-ABC123
nixos-update-manager fresh candidate-ABC123
nixos-update-manager prepare candidate-ABC123
nixos-update-manager cancel candidate-ABC123
nixos-update-manager history
nixos-update-manager review candidate-ABC123
nixos-update-manager apply-plan candidate-ABC123
nixos-update-manager apply candidate-ABC123 --commit --message 'flake: update inputs'
nixos-update-manager commit candidate-ABC123
```

Use `--state-dir /tmp/updates-test` for isolated test state. `--update-input NAME` and `--exclude-input NAME` are repeatable Check options. Defaults advance direct root inputs; follows aliases resolve to their dependency changes. Conflicting shared-node exclusions fail explicitly. Checking copies an already-dirty/staged lock as-is rather than overwriting it.

## Compatibility identifiers

Canonical commands are `nixos-update-manager` and `nixos-update-manager-helper`; the module uses `services.nixos-update-manager`. Legacy binaries remain aliases, as do old leaf options under `services.nixos-updates`.

State remains in `nixos-updates`. Desktop/D-Bus ID `io.github.oddship.NixOSUpdates`, extension UUID `nixos-updates@oddship.github.io`, polkit ID, activation lock, `NIXOS_UPDATES_*` environment variables, and systemd operation names are retained.

## Current restrictions

Testing is limited to x86_64-linux, Nix 2.34.8, pinned unstable Nixpkgs, and GNOME 50.4. Shallow/detached repositories, merge conflicts, submodules, tracked symlinks, non-UTF-8 filenames, absolute path inputs, relative path inputs outside the repository, impure evaluation, and import-from-derivation are unsupported. Required untracked sources must be reviewed and tracked by you; the app never stages them automatically.

Scheduling, retention/history/preferences UI, wider notification/interruption coverage, accessibility, and a stable/unstable release matrix remain unfinished. See [release criteria](RELEASING.md). Contributor build and test instructions are in [CONTRIBUTING.md](../CONTRIBUTING.md).
