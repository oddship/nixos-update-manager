# Implementation status

Updated 2026-10-05. This is a development preview, not a stable release.

## Implemented

- Native folder selection and host discovery, isolated Check, explicit Prepare, grouped package review, and candidate freshness checks.
- Background jobs, cancellation/retry, retained outputs, stale-review refresh, and closed-window notifications that reopen the app.
- Experimental authenticated activation of the exact reviewed system, optional lock-only commit, and commit-only retry with durable Git recovery.
- Optional GNOME 50 status indicator; installation leaves activation to the user.
- Optional Pi explanations, enabled by a successful explicit connection test. Chat and source repair are unavailable.

See [usage and recovery](USAGE.md) for behavior and [architecture](../SPEC.md) for transaction boundaries.

## Validation scope

The app has one build with GUI and CLI entry points, Pi as its only assistant, and one Nix package that includes the privileged helper. The optional GNOME indicator remains separate.

The final single-app source passed formatting, strict Clippy, all 30 Rust tests, and the indicator tests. The app and indicator packages built successfully. Package interface checks verified the CLI, compatibility aliases, and unwrapped helper. All three isolated NixOS VM checks passed, covering exact activation, authenticated helper behavior, Apply/lock-only commit, commit-only retry, and partial activation reporting. Real Nix probes also verified lock isolation, ten package exports, closure review, and Check/Prepare against a synthetic NixOS configuration without host activation.

Earlier pinned x86_64-linux GNOME guest runs exercised Check/Prepare without seeded candidates, real graphical authentication, exact running-system/profile activation, lock-only commits, preservation of unrelated staged/working edits, closed-window notifications, and indicator state changes. These checks do not establish a broad compatibility or crash matrix.

The final packaged app's grouped review and Settings screen were inspected in the native GNOME guest after the rename, using retained synthetic review state. Earlier Settings/explanation checks use a synthetic Pi subprocess, not a real provider. Actual Pi 1.0.0 version and Console launch were checked without provider authentication or model requests. The [walkthrough notes](walkthrough/README.md) explain capture provenance.

The exercised environment uses pinned unstable Nixpkgs, Nix 2.34.8, and GNOME 50.4 on x86_64-linux. Stable NixOS, other architectures, and wider GNOME versions remain unverified.

## Remaining work

- Broader authentication cancellation, partial activation, worker/commit interruption, ref races, and real signing-agent coverage.
- Stable/unstable and supported configuration/version matrices.
- Scheduling, retention, preferences/history UI, and wider notification failure handling.
- Keyboard, narrow-window, light/dark, and accessibility review.
- Real Pi provider/model integration checks; no automated cloud requests are part of the test suite.
- Hosted CI and fresh-install checks before a stable release; the public repository has private vulnerability reporting enabled.

Building the package alone does not close these release gates. See [release criteria](RELEASING.md).
