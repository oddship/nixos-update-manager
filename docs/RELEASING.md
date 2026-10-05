# Release criteria

Version `0.1.0` identifies the development build. Public source availability does not mean a stable product release. [Status](STATUS.md) records the tested scope; this checklist covers the work needed before tagging a usable release.

## Product checks

- Exercise Check → Prepare → Review → graphical authentication → exact Apply → optional lock-only Commit from a fresh installation.
- Verify running-system/profile identity and preservation of unrelated staged/working edits.
- Cover authentication refusal/cancellation, partial activation, worker and commit interruption, ref races, signing failures, and commit-only retry.
- Record a stable/unstable NixOS compatibility matrix and supported GNOME/Nix versions.
- Review closed-window notifications, disabled notifications, scheduling, retention, and supported configuration flows.
- Inspect keyboard navigation, narrow windows, light/dark styles, and accessibility.
- Verify indicator state changes, opening the app, and disable/re-enable on supported Shell versions.
- Validate explicit assistant requests, excluded prompt content, timeouts/failures, verification invalidation, and supported real providers separately from scripted fixtures.
- Resolve wrong-system activation, repository corruption, and false-success defects before release.

The deterministic updater must work without a model. Do not enable source repair or broader agent tools without separate isolation and authority checks.

## Repository checks

- Audit every tracked file and Git history for secrets, private paths, and unintended artifacts. Keep only useful, documented synthetic captures.
- Verify LICENSE, dependency notices, package metadata, desktop integration, and documented installation.
- Run `python3 scripts/check-public-repo.py`, `just lint`, `just test`, `just indicator-test`, the app package build, and relevant disposable VM checks from the intended release commit.
- Run GitHub CI on the public repository; local success does not verify hosted runners.
- Enable private vulnerability reporting and configure branch protection/required CI.
- Keep CI permissions read-only unless a reviewed task needs more. Pin action revisions and review updates.

## Publishing a version

1. Set the version consistently in Cargo.toml, Cargo.lock, and flake.nix. Update CHANGELOG.md with supported versions and known restrictions.
2. Test the exact commit and documented flake/module installation in a fresh guest. Record the Git/Nixpkgs revisions, package output, and checks.
3. Prepare release notes with user-facing changes, installation, tested scope, limitations, and recovery guidance. A lone executable lacks the GTK/Nix dependencies and helper policy supplied by the flake/module.
4. Review the concrete build and release notes before tagging/publishing. Refresh the walkthrough when its age makes the current flow unclear.
5. Verify tag-based installation in a fresh guest after publication.

Do not publish guest disks, state directories, credentials, or unsanitized logs as assets. Document checksums and installation paths for any assets beyond GitHub's generated source archives.
