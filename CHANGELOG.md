# Changelog

No stable release has been published. Version `0.1.0` identifies the development preview.

## Unreleased

- Native GNOME app with folder/host selection, isolated input checks, explicit preparation, grouped package/version review, and stale-candidate handling.
- Background workers, cancellation/retry, retained outputs, and completion notifications that reopen the review.
- Experimental authenticated exact-system Apply, optional lock-only commits, and commit-only retry with Git recovery.
- Optional GNOME 50 status indicator, installed by the module and enabled by the user.
- Optional Pi explanations: an explicit Settings connection test unlocks contextual Explain. Account/model controls and privacy details are collapsed; source repair and chat are unavailable.
- One app with GUI and CLI entry points, Pi as the only assistant, and one Nix package containing the app and privileged helper. The optional GNOME indicator remains separate.
- Pinned Nix environment and justfile with bounded build concurrency and incremental local launch.
- Public name NixOS Update Manager, canonical app/helper commands, and `services.nixos-update-manager`. Legacy binary/module aliases and state/desktop/authorization identities preserve compatibility.

See [status](docs/STATUS.md) for verified scope and [usage](docs/USAGE.md) for restrictions and recovery.
