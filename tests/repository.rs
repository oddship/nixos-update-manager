use nixos_update_manager::{
    backend::{Candidate, InputPolicy, State, Store},
    repository::{Repository, git},
};
use std::{fs, path::Path};

fn fixture(root: &Path) {
    git(root, &["init", "-q"]).unwrap();
    git(root, &["config", "user.name", "Fixture"]).unwrap();
    git(root, &["config", "user.email", "fixture@example.invalid"]).unwrap();
    git(root, &["config", "commit.gpgSign", "false"]).unwrap();
    git(root, &["config", "core.hooksPath", ".git/hooks"]).unwrap();
    fs::write(root.join("flake.nix"), "{}").unwrap();
    fs::write(root.join("flake.lock"), "{}").unwrap();
    fs::write(root.join("module.nix"), "baseline").unwrap();
    fs::write(root.join(".gitignore"), "private\n").unwrap();
    git(root, &["add", "."]).unwrap();
    git(root, &["commit", "-qm", "fixture"]).unwrap();
}

fn persisted_candidate(store: &Store, repo: &Repository, id: &str) -> Candidate {
    fs::create_dir(store.directory(id).unwrap()).unwrap();
    Candidate {
        schema_version: 1,
        id: id.into(),
        repository: repo.clone(),
        fingerprint: repo.fingerprint().unwrap(),
        host: "fixture".into(),
        policy: InputPolicy::default(),
        created_at: 1,
        state: State::Preparing,
        changed_inputs: vec![],
        input_changes: vec![],
        input_packages: vec![],
        installed_input_packages: vec![],
        attribution_error: None,
        skipped_inputs: vec![],
        current_drv: None,
        candidate_drv: Some("/nix/store/fixture.drv".into()),
        candidate_lock_hash: None,
        prepared_system: None,
        review_baseline: None,
        closure_diff: None,
        error: None,
        operation_id: Some("operation-first".into()),
        created_at_millis: 1000,
        application: None,
    }
}

#[test]
fn recovery_preserves_live_workers_and_cancellation_cannot_poison_a_retry() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    fs::create_dir(&root).unwrap();
    fixture(&root);
    let repo = Repository::open(&root).unwrap();
    let before = repo.fingerprint().unwrap();
    let state = temp.path().join("state");
    let store = Store::open(&state).unwrap();
    let mut c = persisted_candidate(&store, &repo, "candidate-cancel");
    store.save(&c).unwrap();
    assert!(Store::open(&state).is_err());
    assert!(matches!(store.load(&c.id).unwrap().state, State::Preparing));
    assert!(Store::request_cancel(&state, &c.id).unwrap());
    assert!(Store::request_cancel(&state, &c.id).unwrap());
    drop(store);
    let store = Store::open(&state).unwrap();
    assert!(matches!(store.load(&c.id).unwrap().state, State::Cancelled));
    assert!(!Store::request_cancel(&state, &c.id).unwrap());

    // Retrying with a new token ignores the previous cancellation marker.
    c.operation_id = Some("operation-second".into());
    store.save(&c).unwrap();
    drop(store);
    let store = Store::open(&state).unwrap();
    let recovered = store.load(&c.id).unwrap();
    assert!(matches!(recovered.state, State::Failed));
    assert!(recovered.operation_id.is_none());
    assert!(recovered.candidate_drv.is_some());

    // Legacy interrupted records lacking a token also recover safely.
    let mut legacy = persisted_candidate(&store, &repo, "candidate-legacy");
    legacy.state = State::Checking;
    let mut record = serde_json::to_value(&legacy).unwrap();
    record.as_object_mut().unwrap().remove("operation_id");
    fs::write(
        store.directory(&legacy.id).unwrap().join("state.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    drop(store);
    let store = Store::open(&state).unwrap();
    assert!(matches!(
        store.load(&legacy.id).unwrap().state,
        State::Failed
    ));
    assert_eq!(repo.fingerprint().unwrap(), before);
}

#[test]
fn history_filters_before_selecting_and_reads_while_worker_is_locked() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    fs::create_dir(&root).unwrap();
    fixture(&root);
    let repo = Repository::open(&root).unwrap();
    let state = temp.path().join("state");
    let store = Store::open(&state).unwrap();
    let mut first = persisted_candidate(&store, &repo, "candidate-first");
    first.state = State::UpdatesFound;
    first.created_at_millis = 1100;
    store.save(&first).unwrap();
    let mut second = persisted_candidate(&store, &repo, "candidate-second");
    second.host = "other".into();
    second.created_at_millis = 1900;
    store.save(&second).unwrap();
    let mut third = persisted_candidate(&store, &repo, "candidate-third");
    third.created_at_millis = 1500;
    store.save(&third).unwrap();
    assert!(Store::open(&state).is_err());
    assert!(matches!(
        Store::read(&state, &third.id).unwrap().state,
        State::Preparing
    ));
    assert_eq!(
        Store::latest_for(&state, &root, "fixture").unwrap().id,
        third.id
    );
    assert_eq!(
        Store::latest_for(&state, &root, "other").unwrap().id,
        second.id
    );
    assert!(Store::latest_for(&state, &root, "absent").is_none());
    assert_eq!(Store::records(&state).unwrap().len(), 3);
    std::os::unix::fs::symlink(
        store.directory(&first.id).unwrap(),
        state.join("candidate-alias"),
    )
    .unwrap();
    assert!(Store::read(&state, "candidate-alias").is_err());
    assert_eq!(Store::records(&state).unwrap().len(), 3);
}

#[test]
fn exact_lock_commit_preserves_unrelated_staging_and_honors_hooks() {
    use nixos_update_manager::{commit::commit_lock, repository::hash};
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    fs::write(temp.path().join("module.nix"), b"staged").unwrap();
    git(temp.path(), &["add", "module.nix"]).unwrap();
    fs::write(temp.path().join("module.nix"), b"unstaged over staged").unwrap();
    let staged = git(temp.path(), &["show", ":module.nix"]).unwrap();
    fs::write(temp.path().join("flake.lock"), b"candidate").unwrap();
    let hook = temp.path().join(".git/hooks/pre-commit");
    fs::write(
        &hook,
        "#!/bin/sh\ntest \"$(git diff --cached --name-only)\" = flake.lock\n",
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let repo = Repository::open(temp.path()).unwrap();
    let expected = repo.fingerprint().unwrap();
    let state = tempfile::tempdir().unwrap();
    let outcome = commit_lock(
        &repo,
        &expected,
        &hash(b"{}"),
        &hash(b"candidate"),
        "flake: fixture update",
        state.path(),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(git(temp.path(), &["rev-parse", "HEAD"]).unwrap())
            .unwrap()
            .trim(),
        outcome.commit
    );
    assert_eq!(
        git(temp.path(), &["show", "HEAD:flake.lock"]).unwrap(),
        b"candidate"
    );
    assert_eq!(
        git(temp.path(), &["show", "HEAD:module.nix"]).unwrap(),
        b"baseline"
    );
    assert_eq!(git(temp.path(), &["show", ":module.nix"]).unwrap(), staged);
    assert_eq!(
        fs::read(temp.path().join("module.nix")).unwrap(),
        b"unstaged over staged"
    );
    assert_eq!(
        git(temp.path(), &["show", ":flake.lock"]).unwrap(),
        b"candidate"
    );
    assert!(!temp.path().join(".git/index.lock").exists());
    assert!(!temp.path().join(".git/HEAD.lock").exists());
}

#[test]
fn activation_freshness_allows_only_the_reviewed_lock_write() {
    use nixos_update_manager::repository::hash;
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let repo = Repository::open(temp.path()).unwrap();
    let original = repo.fingerprint().unwrap();
    fs::write(temp.path().join("flake.lock"), b"candidate").unwrap();
    let after = repo
        .require_only_lock_change(&original, b"{}", &hash(b"candidate"))
        .unwrap();
    assert_eq!(after.index_hash, original.index_hash);
    fs::write(temp.path().join("module.nix"), b"concurrent source edit").unwrap();
    assert!(
        repo.require_only_lock_change(&original, b"{}", &hash(b"candidate"))
            .is_err()
    );
    fs::write(temp.path().join("module.nix"), b"baseline").unwrap();
    git(temp.path(), &["add", "flake.lock"]).unwrap();
    assert!(
        repo.require_only_lock_change(&original, b"{}", &hash(b"candidate"))
            .is_err()
    );
}

#[test]
fn hook_cannot_add_unreviewed_commit_contents() {
    use nixos_update_manager::{commit::commit_lock, repository::hash};
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    fs::write(temp.path().join("flake.lock"), b"candidate").unwrap();
    fs::write(temp.path().join("module.nix"), b"local edit").unwrap();
    let hook = temp.path().join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\ngit add module.nix\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let repo = Repository::open(temp.path()).unwrap();
    let before = repo.fingerprint().unwrap();
    let state = tempfile::tempdir().unwrap();
    let error = commit_lock(
        &repo,
        &before,
        &hash(b"{}"),
        &hash(b"candidate"),
        "flake: fixture update",
        state.path(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("commit tree"));
    assert_eq!(repo.fingerprint().unwrap(), before);
}

#[test]
fn snapshot_preserves_index_and_dirty_bytes_without_untracked_or_ignored_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    fs::create_dir(&root).unwrap();
    fixture(&root);
    fs::write(root.join("module.nix"), "staged").unwrap();
    git(&root, &["add", "module.nix"]).unwrap();
    fs::write(root.join("module.nix"), "unstaged over staged").unwrap();
    fs::write(root.join("private"), "secret fixture").unwrap();
    fs::write(root.join("untracked"), "untracked fixture").unwrap();
    let repo = Repository::open(&root).unwrap();
    let expected = repo.fingerprint().unwrap();
    let copied = repo
        .snapshot(&temp.path().join("candidate"), &expected)
        .unwrap();
    assert_eq!(
        fs::read_to_string(copied.join("module.nix")).unwrap(),
        "unstaged over staged"
    );
    assert!(!copied.join("private").exists());
    assert!(!copied.join("untracked").exists());
    assert_eq!(
        git(&root, &["ls-files", "--stage"]).unwrap(),
        git(&copied, &["ls-files", "--stage"]).unwrap()
    );
    assert_eq!(repo.fingerprint().unwrap(), expected);
    fs::write(root.join("module.nix"), "concurrent edit").unwrap();
    assert!(
        repo.snapshot(&temp.path().join("stale"), &expected)
            .is_err()
    );
}

#[test]
fn state_lock_and_candidate_path_validation_prevent_overlapping_jobs_and_traversal() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    assert!(Store::open(temp.path()).is_err());
    assert!(store.directory("candidate-../../outside").is_err());
    assert!(store.directory("candidate-123Ab").is_ok());
    drop(store);
    assert!(Store::open(temp.path()).is_ok());
}

#[test]
fn a_tracked_symlink_is_rejected_instead_of_followed() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    std::os::unix::fs::symlink("/etc/passwd", temp.path().join("link")).unwrap();
    git(temp.path(), &["add", "link"]).unwrap();
    assert!(Repository::open(temp.path()).is_err());
}

#[test]
fn cli_cancels_a_live_check_without_the_worker_lock_and_next_check_succeeds() {
    use std::{
        os::unix::fs::PermissionsExt,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    fs::create_dir(&root).unwrap();
    fixture(&root);
    fs::write(root.join("flake.lock"), r#"{"version":7,"root":"root","nodes":{"root":{"inputs":{"dep":"dep"}},"dep":{"locked":{"type":"git"}}}}"#).unwrap();
    git(&root, &["add", "flake.lock"]).unwrap();
    git(&root, &["commit", "-qm", "lock fixture"]).unwrap();
    let repo = Repository::open(&root).unwrap();
    let before = repo.fingerprint().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let started = temp.path().join("started");
    let orphan = temp.path().join("orphan");
    let shim = bin.join("nix");
    fs::write(&shim, "#!/bin/sh\nif [ \"$FIXTURE_BLOCK\" = 1 ]; then\n  printf started > \"$FIXTURE_STARTED\"\n  (sleep 1; printf orphan > \"$FIXTURE_ORPHAN\") &\n  wait\nelse\n  case \"$*\" in *builtins.attrNames*) printf '[\"fixture\"]';; esac\nfi\n").unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    let state = temp.path().join("state");
    let command = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nixos-update-manager"));
        cmd.arg("--state-dir")
            .arg(&state)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("FIXTURE_STARTED", &started)
            .env("FIXTURE_ORPHAN", &orphan);
        cmd
    };
    let mut worker = command()
        .arg("check")
        .arg(&root)
        .args(["--host", "fixture"])
        .env("FIXTURE_BLOCK", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !started.exists() {
        if Instant::now() > deadline {
            let _ = worker.kill();
            let _ = worker.wait();
            panic!("fixture worker never started");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let id = fs::read_dir(&state)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|name| name.starts_with("candidate-"))
        .unwrap();
    let shown = command().args(["show", &id]).output().unwrap();
    assert!(
        shown.status.success(),
        "show must not wait for the worker lock"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&shown.stdout).unwrap()["state"],
        "checking"
    );
    let cancel = command().args(["cancel", &id]).output().unwrap();
    assert!(
        cancel.status.success(),
        "{}",
        String::from_utf8_lossy(&cancel.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&cancel.stdout).unwrap()["requested"],
        true
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while worker.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = worker.kill();
            let _ = worker.wait();
            panic!("worker did not honour cancellation");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let result = worker.wait_with_output().unwrap();
    assert!(!result.status.success());
    let cancelled: Candidate = serde_json::from_slice(&result.stdout).unwrap();
    assert!(matches!(cancelled.state, State::Cancelled));
    assert!(cancelled.operation_id.is_none());
    let next = command()
        .arg("check")
        .arg(&root)
        .args(["--host", "fixture"])
        .env("FIXTURE_BLOCK", "0")
        .output()
        .unwrap();
    assert!(
        next.status.success(),
        "{}",
        String::from_utf8_lossy(&next.stderr)
    );
    let completed: Candidate = serde_json::from_slice(&next.stdout).unwrap();
    assert!(matches!(completed.state, State::UpToDate));
    thread::sleep(Duration::from_millis(1200));
    assert!(!orphan.exists());
    assert_eq!(repo.fingerprint().unwrap(), before);
}

#[test]
fn signing_failure_preserves_the_reviewed_checkout_and_real_index() {
    use nixos_update_manager::{commit::commit_lock, repository::hash};
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let state = tempfile::tempdir().unwrap();
    let signer = state.path().join("refuse-signature");
    fs::write(
        &signer,
        "#!/bin/sh\necho 'fixture signer refused' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&signer, fs::Permissions::from_mode(0o755)).unwrap();
    git(temp.path(), &["config", "commit.gpgSign", "true"]).unwrap();
    git(temp.path(), &["config", "gpg.format", "ssh"]).unwrap();
    git(
        temp.path(),
        &["config", "user.signingKey", "key::ssh-ed25519 AAAAfixture"],
    )
    .unwrap();
    git(
        temp.path(),
        &["config", "gpg.ssh.program", signer.to_str().unwrap()],
    )
    .unwrap();
    fs::write(temp.path().join("flake.lock"), b"candidate").unwrap();
    let repo = Repository::open(temp.path()).unwrap();
    let before = repo.fingerprint().unwrap();
    let error = commit_lock(
        &repo,
        &before,
        &hash(b"{}"),
        &hash(b"candidate"),
        "fixture",
        state.path(),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("fixture signer refused"));
    assert_eq!(repo.fingerprint().unwrap(), before);
    assert!(!temp.path().join(".git/index.lock").exists());
    assert!(!temp.path().join(".git/HEAD.lock").exists());
    assert!(!state.path().join("commit-intent.json").exists());
}

#[test]
fn state_inside_the_configuration_repository_is_rejected_before_checking() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let repo = Repository::open(temp.path()).unwrap();
    let store = Store::open(&temp.path().join("updater-state")).unwrap();
    let before = repo.fingerprint().unwrap();
    let error = store
        .check(temp.path(), "fixture", InputPolicy::default())
        .unwrap_err();
    assert!(error.to_string().contains("outside"));
    assert_eq!(repo.fingerprint().unwrap(), before);
    assert!(
        Store::records(&temp.path().join("updater-state"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn commit_recovery_repairs_only_the_recorded_index_after_ref_publication() {
    use nixos_update_manager::{
        commit::{commit_lock, recover_commit},
        repository::hash,
    };
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    fs::write(temp.path().join("module.nix"), b"staged unrelated").unwrap();
    git(temp.path(), &["add", "module.nix"]).unwrap();
    fs::write(temp.path().join("module.nix"), b"working over staged").unwrap();
    fs::write(temp.path().join("flake.lock"), b"candidate").unwrap();
    let original_index = fs::read(temp.path().join(".git/index")).unwrap();
    let repo = Repository::open(temp.path()).unwrap();
    let expected = repo.fingerprint().unwrap();
    let state = tempfile::tempdir().unwrap();
    let outcome = commit_lock(
        &repo,
        &expected,
        &hash(b"{}"),
        &hash(b"candidate"),
        "fixture",
        state.path(),
    )
    .unwrap();
    // Simulate the durable ref succeeding just before the real index was replaced.
    fs::write(temp.path().join(".git/index"), &original_index).unwrap();
    let recovered = recover_commit(&repo, state.path()).unwrap().unwrap();
    assert_eq!(recovered.commit, outcome.commit);
    assert!(recovered.post_commit_warning.is_some());
    assert_eq!(
        git(temp.path(), &["show", ":flake.lock"]).unwrap(),
        b"candidate"
    );
    assert_eq!(
        git(temp.path(), &["show", ":module.nix"]).unwrap(),
        b"staged unrelated"
    );
    assert_eq!(
        fs::read(temp.path().join("module.nix")).unwrap(),
        b"working over staged"
    );
    assert_eq!(
        recover_commit(&repo, state.path()).unwrap().unwrap().commit,
        outcome.commit
    );
    // An external index edit must never be overwritten by the recovery record.
    fs::write(temp.path().join("module.nix"), b"external staging").unwrap();
    git(temp.path(), &["add", "module.nix"]).unwrap();
    let changed = repo.fingerprint().unwrap();
    assert!(recover_commit(&repo, state.path()).is_err());
    assert_eq!(repo.fingerprint().unwrap(), changed);
}

#[test]
fn unpublished_commit_intent_can_retry_but_foreign_git_locks_are_preserved() {
    use nixos_update_manager::{
        commit::{commit_lock, recover_commit},
        repository::hash,
    };
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path());
    fs::write(temp.path().join("flake.lock"), b"candidate").unwrap();
    let index = fs::read(temp.path().join(".git/index")).unwrap();
    let repo = Repository::open(temp.path()).unwrap();
    let expected = repo.fingerprint().unwrap();
    let state = tempfile::tempdir().unwrap();
    commit_lock(
        &repo,
        &expected,
        &hash(b"{}"),
        &hash(b"candidate"),
        "fixture",
        state.path(),
    )
    .unwrap();
    // Replay the recorded boundary before publication, without rerunning hooks.
    git(temp.path(), &["update-ref", "HEAD", &expected.head]).unwrap();
    fs::write(temp.path().join(".git/index"), &index).unwrap();
    fs::write(
        temp.path().join(".git/index.lock"),
        b"another Git operation",
    )
    .unwrap();
    assert!(recover_commit(&repo, state.path()).is_err());
    assert_eq!(
        fs::read(temp.path().join(".git/index.lock")).unwrap(),
        b"another Git operation"
    );
    fs::remove_file(temp.path().join(".git/index.lock")).unwrap();
    assert!(recover_commit(&repo, state.path()).unwrap().is_none());
    assert!(!state.path().join("commit-intent.json").exists());
    assert_eq!(repo.fingerprint().unwrap(), expected);
    commit_lock(
        &repo,
        &expected,
        &hash(b"{}"),
        &hash(b"candidate"),
        "retry fixture",
        state.path(),
    )
    .unwrap();
}

#[test]
fn reopening_the_backend_reconciles_a_published_commit_without_repeating_hooks() {
    use nixos_update_manager::{apply::ApplicationRecord, commit::commit_lock, repository::hash};
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    fs::create_dir(&root).unwrap();
    fixture(&root);
    let hook_count = temp.path().join("hook-count");
    let hook = root.join(".git/hooks/post-commit");
    fs::write(
        &hook,
        format!("#!/bin/sh\nprintf done >> '{}'\n", hook_count.display()),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let repo = Repository::open(&root).unwrap();
    let state = temp.path().join("state");
    let store = Store::open(&state).unwrap();
    let mut c = persisted_candidate(&store, &repo, "candidate-commit-recovery");
    let attempt = store.directory(&c.id).unwrap().join("application");
    fs::create_dir(&attempt).unwrap();
    fs::write(root.join("flake.lock"), b"candidate").unwrap();
    let applied = repo.fingerprint().unwrap();
    let index = fs::read(root.join(".git/index")).unwrap();
    let outcome = commit_lock(
        &repo,
        &applied,
        &hash(b"{}"),
        &hash(b"candidate"),
        "fixture",
        &attempt,
    )
    .unwrap();
    fs::write(root.join(".git/index"), &index).unwrap();
    c.state = State::Committing;
    c.application = Some(ApplicationRecord {
        directory: attempt,
        acknowledged: true,
        activation_completed: true,
        fingerprint: Some(applied),
        actual: None,
        commit: None,
        message: Some("fixture".into()),
    });
    store.save(&c).unwrap();
    drop(store);
    let store = Store::open(&state).unwrap();
    let recovered = store.load(&c.id).unwrap();
    assert!(matches!(recovered.state, State::Committed));
    assert_eq!(
        recovered.application.unwrap().commit.unwrap(),
        outcome.commit
    );
    assert_eq!(git(&root, &["show", ":flake.lock"]).unwrap(), b"candidate");
    assert_eq!(fs::read(hook_count).unwrap(), b"done");
}

#[test]
fn legacy_success_without_activation_completion_is_not_trusted_for_commits() {
    use nixos_update_manager::apply::ApplicationRecord;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    fs::create_dir(&root).unwrap();
    fixture(&root);
    let repo = Repository::open(&root).unwrap();
    let state = temp.path().join("state");
    let store = Store::open(&state).unwrap();
    let mut c = persisted_candidate(&store, &repo, "candidate-legacy-applied");
    c.state = State::Applied;
    c.application = Some(ApplicationRecord {
        directory: store.directory(&c.id).unwrap().join("application"),
        acknowledged: true,
        activation_completed: false,
        fingerprint: Some(repo.fingerprint().unwrap()),
        actual: None,
        commit: None,
        message: Some("must not commit".into()),
    });
    store.save(&c).unwrap();
    let before = repo.fingerprint().unwrap();
    assert!(store.commit(&c.id).is_err());
    drop(store);
    let store = Store::open(&state).unwrap();
    assert!(matches!(
        store.load(&c.id).unwrap().state,
        State::ApplyNeedsAttention
    ));
    assert_eq!(repo.fingerprint().unwrap(), before);
}

#[test]
fn empty_commit_message_is_rejected_before_authentication_or_checkout_writes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    fs::create_dir(&root).unwrap();
    fixture(&root);
    let repo = Repository::open(&root).unwrap();
    let store = Store::open(&temp.path().join("state")).unwrap();
    let mut c = persisted_candidate(&store, &repo, "candidate-empty-message");
    c.state = State::Ready;
    store.save(&c).unwrap();
    let before = repo.fingerprint().unwrap();
    let error = store.apply(&c.id, true, Some("  ".into())).unwrap_err();
    assert!(error.to_string().contains("commit message is empty"));
    assert!(matches!(store.load(&c.id).unwrap().state, State::Ready));
    assert!(store.load(&c.id).unwrap().application.is_none());
    assert_eq!(repo.fingerprint().unwrap(), before);
    assert_eq!(
        fs::read_dir(store.directory(&c.id).unwrap())
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn older_candidate_records_without_input_revisions_remain_readable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    fs::create_dir(&root).unwrap();
    fixture(&root);
    let repo = Repository::open(&root).unwrap();
    let store = Store::open(&temp.path().join("state")).unwrap();
    let candidate = persisted_candidate(&store, &repo, "candidate-old");
    let mut value = serde_json::to_value(&candidate).unwrap();
    for field in [
        "input_changes",
        "input_packages",
        "installed_input_packages",
        "attribution_error",
    ] {
        value.as_object_mut().unwrap().remove(field);
    }
    fs::write(
        store.directory(&candidate.id).unwrap().join("state.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    assert!(store.load(&candidate.id).unwrap().input_changes.is_empty());
}

#[test]
fn pi_receives_only_the_review_summary_and_runs_without_project_resources() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("private-repository");
    fs::create_dir(&root).unwrap();
    fixture(&root);
    let repo = Repository::open(&root).unwrap();
    let store = Store::open(&temp.path().join("state")).unwrap();
    let mut candidate = persisted_candidate(&store, &repo, "candidate-pi");
    candidate.state = State::Ready;
    candidate.changed_inputs = vec!["tools".into()];
    candidate.closure_diff = Some("app: 1 → 2\n".into());
    candidate.error = Some("private-diagnostic-marker".into());
    let engine = temp.path().join("fake-pi");
    let python = std::process::Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let interpreter = String::from_utf8(python.stdout).unwrap();
    let script = format!("#!{}\n", interpreter.trim())
        + r#"
import json, os, sys
flags = sys.argv[1:]
for flag in ['--no-tools', '--no-extensions', '--no-skills', '--no-prompt-templates', '--no-context-files', '--no-session', '--no-approve', '--print', '--offline']:
    assert flag in flags, flag
assert flags[flags.index('--mode') + 1] == 'json'
assert flags[flags.index('--model') + 1] == 'test/model'
assert 'NIXOS_UPDATES_HELPER' not in os.environ
assert not os.path.exists('flake.nix')
prompt = sys.stdin.read()
assert 'private-repository' not in prompt and 'private-diagnostic-marker' not in prompt
is_test = prompt == 'This is a connection test. Reply only with Connected.'
if not is_test:
    context = json.loads(prompt.split('\n', 1)[1])
    assert context['inputs'] == ['tools']
    assert context['packages'] == [{'name':'app', 'change':'1 → 2'}]
print(json.dumps({'type':'message_end', 'message':{'role':'assistant', 'stopReason':'stop', 'content':[{'type':'text','text':'Connected' if is_test else 'App updates from 1 to 2.'}]}}))
print(json.dumps({'type':'agent_settled'}))
"#;
    fs::write(&engine, script).unwrap();
    fs::set_permissions(&engine, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        nixos_update_manager::pi::test_connection(&engine, "test/model").unwrap(),
        "Connected"
    );
    assert_eq!(
        nixos_update_manager::pi::explain(&engine, "test/model", &candidate).unwrap(),
        "App updates from 1 to 2."
    );
}
