# Architecture and update contract

NixOS Update Manager is a GTK4/libadwaita frontend and Rust backend for a local Git-managed NixOS flake. The GUI and JSON CLI are entry points in the same app and share candidate and transaction logic. One Nix package contains the app, Pi, and a separate privileged helper executable; packaging them together does not merge their authority. This document describes implemented boundaries; [status](docs/STATUS.md) lists validation and remaining work.

## Candidate isolation

Checking uses an app-owned Git-filtered snapshot, including tracked working edits and excluding ignored/untracked files. The candidate lock is installed in the snapshot before evaluation. Checking and preparation never replace the checkout's lock or index.

The backend records repository, host, source/lock/index/HEAD fingerprints, input policy, evaluated derivation, prepared output, and review baseline. Atomic private records and operation locks coordinate workers. Concurrent changes invalidate the candidate rather than silently changing its meaning.

Checks disable import-from-derivation and do not intentionally build the system. Host enumeration avoids evaluating unrelated host outputs. Preparation builds the recorded derivation, retains it and its output with GC roots, and compares the candidate closure with the running system.

## Review

Input repository revisions are distinct from package versions. Review rows show actual closure version sets, additions, removals, unversioned entries, and size changes. Raw Nix review remains available separately.

At Check, at most 512 explicitly exported `input.packages.<system>` attributes per input are evaluated without realization. Prepare filters these against the candidate closure. Grouping requires an exact package-name/new-version match and one unique input; aliases are deduplicated. Ambiguous, removed, overlay-only, and unmapped changes remain Other packages and system components. Attribution is deliberately incomplete.

A changed running system invalidates the runtime review. Refresh compares the retained candidate against the new baseline without rebuilding, while rechecking source freshness.

## Activation authority

GTK and the backend remain unprivileged. The NixOS module installs the small authenticated root helper and polkit policy. The helper accepts the prepared system and baseline through the defined protocol; it does not evaluate the live checkout or run a new build.

Apply revalidates freshness, authenticates, writes only the reviewed lock, and activates the retained system. Root activation uses a global lease. Lock writes have durable transaction records. Successful application requires matching running-system/profile paths and backend-recorded successful helper completion. Matching links alone cannot establish success after a failed activation.

Partial activation is an attention state, not a promise of rollback. Restoring a lock file does not restore changed services. Preserve recovery records and report uncertainty rather than automatically committing.

## Git transaction

Optional commits contain only the reviewed lock change. The backend uses an isolated index, validates the exact tree, honors hooks and configured signing, and preserves unrelated staged/working changes. It never stashes, resets, stages unrelated files, changes branches, or pushes automatically.

Commit intent records the expected source fingerprint and exact replacement index. Recovery validates commit/tree/parent/lock and original-or-final index contents before repair. A retained hard link proves ownership of stale Git locks without relying on inode numbers alone. External changes or foreign locks prevent automatic reconciliation. Commit-only retry does not reactivate the system.

A lock-only commit does not record tracked local edits included in the built system. It is not a complete reproducibility record of that candidate.

## Desktop workers and indicator

Check/Prepare run as systemd user jobs independent of the window, with lower scheduling priority. Operation tokens scope cancellation and retries; command process groups support cleanup. Durable per-candidate/result announcement records avoid duplicate notifications when reopening.

The optional GNOME 50 extension reads default app state and opens the registered app. It does not run Nix, Git, the helper, or activation. Reads are bounded to 1 MiB per record and 256 history records; larger histories show neutral status. File events are coalesced, with a 15-second fallback retry.

Compatibility identifiers are intentionally retained after the rename. See [usage](docs/USAGE.md#compatibility-identifiers).

## Optional assistant

The default package includes pinned Pi 1.0. Settings tests an existing account/model with a fixed short request, only on a click, with no update context and a 60-second deadline. Only successful verification atomically enables contextual explanation for that executable/model. Override edits invalidate it; stale test results cannot re-enable a changed model. External Pi account/model files are not monitored.

Explanation sends only phase, input names, and at most 200 package changes. It excludes files, paths, hashes, error logs, and copied credentials. Pi runs from a neutral directory with tools, extensions, skills, prompt templates, context files, session persistence, and automatic approval disabled. Output is bounded, the deadline is 120 seconds, and the process group is cleaned up. These process controls are not an OS sandbox.

Success requires a successful assistant completion and settled event, not just process exit. Failure clears verification. Model output cannot establish build, activation, or security outcomes. No chat or source repair is implemented.

## Tests and future work

Use synthetic repositories and disposable guests. Backend tests cover isolation, freshness, diffs, cancellation, and Git recovery. VM tests cover helper/activation boundaries; native guest inspection covers notifications, graphical authentication, and presentation. Scripted Pi fixtures validate app behavior without provider credentials or cloud requests.

Scheduling, retention/history/preferences UI, broader version/platform support, interruption/signing matrices, accessibility, and real provider validation remain unfinished. Agent repair needs separate isolation and authority design before implementation. See [release criteria](docs/RELEASING.md).
