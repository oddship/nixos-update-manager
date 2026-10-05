//! Exact lock-only commit plumbing, isolated from activation.
use crate::{
    process::{capture, dialogue},
    repository::{Fingerprint, Repository, git, hash},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct CommitOutcome {
    pub commit: String,
    pub post_commit_warning: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CommitIntent {
    schema_version: u32,
    expected: Fingerprint,
    commit: String,
    tree: String,
    lock_blob: String,
    next_index_hash: String,
    index_lock_device: u64,
    index_lock_inode: u64,
    index_lock_anchor: PathBuf,
}

fn durable_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("record directory missing")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

struct Lease {
    path: PathBuf,
    file: File,
    anchor: Option<PathBuf>,
}
impl Lease {
    fn acquire(path: PathBuf) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .context("Git is busy or has an existing lock; do not delete its lock automatically")?;
        Ok(Self {
            path,
            file,
            anchor: None,
        })
    }

    fn anchor(&mut self) -> Result<PathBuf> {
        let parent = self.path.parent().context("Git lock directory missing")?;
        let temporary = tempfile::Builder::new()
            .prefix(".nixos-updates-lease-")
            .tempfile_in(parent)?;
        fs::remove_file(temporary.path())?;
        fs::hard_link(&self.path, temporary.path())?;
        let (_, anchor) = temporary.keep()?;
        File::open(parent)?.sync_all()?;
        self.anchor = Some(anchor.clone());
        Ok(anchor)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let (Ok(path), Ok(owned)) = (fs::symlink_metadata(&self.path), self.file.metadata())
            && path.dev() == owned.dev()
            && path.ino() == owned.ino()
        {
            let _ = fs::remove_file(&self.path);
        }
        if let Some(anchor) = &self.anchor
            && let (Ok(meta), Ok(owned)) = (fs::symlink_metadata(anchor), self.file.metadata())
            && meta.dev() == owned.dev()
            && meta.ino() == owned.ino()
        {
            let _ = fs::remove_file(anchor);
        }
    }
}

fn text(bytes: Vec<u8>) -> Result<String> {
    Ok(String::from_utf8(bytes)?.trim().into())
}
fn git_path(repo: &Repository, name: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(text(git(
        &repo.root,
        &["rev-parse", "--path-format=absolute", "--git-path", name],
    )?)?))
}

fn with_index(repo: &Repository, index: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(&repo.root)
        .args(args)
        .env("GIT_INDEX_FILE", index)
        .env("GIT_OPTIONAL_LOCKS", "0");
    for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"] {
        cmd.env_remove(name);
    }
    capture(&mut cmd, Duration::from_secs(120))
}

fn hook(repo: &Repository, index: &Path, name: &str, args: &[&Path]) -> Result<()> {
    let default = git_path(repo, "hooks")?;
    let configured = text(git(
        &repo.root,
        &[
            "config",
            "--path",
            "--default",
            default.to_str().context("hook path encoding")?,
            "--get",
            "core.hooksPath",
        ],
    )?)?;
    let path = PathBuf::from(configured);
    let directory = if path.is_absolute() {
        path
    } else {
        repo.root.join(path)
    };
    let executable = directory.join(name);
    if !executable.exists() || fs::metadata(&executable)?.mode() & 0o111 == 0 {
        return Ok(());
    }
    let mut cmd = Command::new(executable);
    cmd.current_dir(&repo.root)
        .args(args)
        .env("GIT_INDEX_FILE", index)
        .env("GIT_EDITOR", ":");
    for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"] {
        cmd.env_remove(name);
    }
    capture(&mut cmd, Duration::from_secs(120)).with_context(|| format!("{name} hook failed"))?;
    Ok(())
}

/// Caller must persist the application/commit intent before invoking this.
/// The initial release disallows pre-existing staged or dirty lock edits.
pub fn commit_lock(
    repo: &Repository,
    expected: &Fingerprint,
    original_lock_hash: &str,
    reviewed_lock_hash: &str,
    message: &str,
    directory: &Path,
) -> Result<CommitOutcome> {
    ensure!(!message.trim().is_empty(), "commit message is empty");
    ensure!(
        &repo.fingerprint()? == expected,
        "repository changed before commit"
    );
    let relative = repo.lock_relative.to_str().context("lock path encoding")?;
    ensure!(
        hash(&git(&repo.root, &["show", &format!("HEAD:{relative}")])?) == original_lock_hash
            && hash(&git(&repo.root, &["show", &format!(":{relative}")])?) == original_lock_hash,
        "automatic commit is disabled for a pre-existing dirty or staged lock"
    );
    ensure!(
        hash(&fs::read(repo.root.join(relative))?) == reviewed_lock_hash,
        "lock does not match the reviewed candidate"
    );
    let index_path = git_path(repo, "index")?;
    let mut index_lease = Lease::acquire(index_path.with_extension("lock"))?;
    let head_lease = Lease::acquire(git_path(repo, "HEAD")?.with_extension("lock"))?;
    ensure!(
        &repo.fingerprint()? == expected,
        "repository changed while acquiring Git locks"
    );
    let scratch = tempfile::Builder::new()
        .prefix("commit-")
        .tempdir_in(directory)?;
    let index = scratch.path().join("index");
    with_index(repo, &index, &["read-tree", &expected.head])?;
    let blob = text(git(&repo.root, &["hash-object", "-w", "--", relative])?)?;
    let mode = text(git(&repo.root, &["ls-files", "--stage", "--", relative])?)?;
    let mode = mode
        .split_whitespace()
        .next()
        .context("lock index entry missing")?;
    with_index(
        repo,
        &index,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            mode,
            &blob,
            relative,
        ],
    )?;
    let message_file = scratch.path().join("message");
    fs::write(&message_file, format!("{}\n", message.trim()))?;
    hook(repo, &index, "pre-commit", &[])?;
    hook(
        repo,
        &index,
        "prepare-commit-msg",
        &[&message_file, Path::new("message")],
    )?;
    hook(repo, &index, "commit-msg", &[&message_file])?;
    ensure!(
        &repo.fingerprint()? == expected,
        "a hook changed the checkout, index, branch or HEAD; commit stopped"
    );
    let tree = text(with_index(repo, &index, &["write-tree"])?)?;
    let changed = with_index(
        repo,
        &index,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-z",
            "-r",
            &expected.head,
            &tree,
        ],
    )?;
    ensure!(
        changed == [relative.as_bytes(), b"\0"].concat(),
        "hook changed the intended commit tree; commit stopped"
    );
    let staged_blob = text(with_index(
        repo,
        &index,
        &["rev-parse", &format!("{tree}:{relative}")],
    )?)?;
    ensure!(
        staged_blob == blob,
        "hook changed the reviewed lock content; commit stopped"
    );
    let sign = text(git(
        &repo.root,
        &[
            "config",
            "--type=bool",
            "--default",
            "false",
            "--get",
            "commit.gpgSign",
        ],
    )?)? == "true";
    let message_path = message_file.to_str().context("message path encoding")?;
    let mut args = vec![
        "commit-tree",
        &tree,
        "-p",
        &expected.head,
        "-F",
        message_path,
    ];
    if sign {
        args.push("-S");
    }
    let commit = text(with_index(repo, &index, &args)?)?;
    // Build a replacement real index from its exact original bytes; unrelated
    // staged entries remain intact. The real index stays locked through CAS.
    let next_index = scratch.path().join("next-index");
    fs::copy(&index_path, &next_index)?;
    with_index(
        repo,
        &next_index,
        &["update-index", "--cacheinfo", mode, &blob, relative],
    )?;
    ensure!(
        &repo.fingerprint()? == expected,
        "repository changed during signing; commit stopped"
    );
    // Persist intent before the atomic ref update, allowing a future reconciler
    // to distinguish ref success from a subsequent index-write failure.
    let next_bytes = fs::read(&next_index)?;
    durable_file(&directory.join("commit-next-index"), &next_bytes)?;
    // A retained hard link prevents inode reuse from impersonating our stale lock.
    let anchor = index_lease.anchor()?;
    let lease_meta = index_lease.file.metadata()?;
    let intent = CommitIntent {
        schema_version: 1,
        expected: expected.clone(),
        commit: commit.clone(),
        tree: tree.clone(),
        lock_blob: blob.clone(),
        next_index_hash: hash(&next_bytes),
        index_lock_device: lease_meta.dev(),
        index_lock_inode: lease_meta.ino(),
        index_lock_anchor: anchor,
    };
    durable_file(
        &directory.join("commit-intent.json"),
        &serde_json::to_vec_pretty(&intent)?,
    )?;
    drop(head_lease);
    // Preparing an update through HEAD locks the symbolic HEAD and its referent.
    // Validate which branch Git locked before sending the commit instruction.
    let commands = format!("start\nupdate HEAD {} {}\nprepare\n", commit, expected.head);
    let mut update = Command::new("git");
    update.arg("-C").arg(&repo.root).args([
        "update-ref",
        "-m",
        "nixos-update-manager: lock update",
        "--stdin",
    ]);
    for name in [
        "GIT_INDEX_FILE",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
    ] {
        update.env_remove(name);
    }
    dialogue(
        &mut update,
        Duration::from_secs(120),
        commands.as_bytes(),
        "prepare: ok\n",
        b"commit\n",
        || {
            ensure!(
                text(git(&repo.root, &["symbolic-ref", "-q", "HEAD"])?)? == expected.branch,
                "Git prepared an unexpected branch; transaction aborted"
            );
            ensure!(
                &repo.fingerprint()? == expected,
                "repository changed before publishing the commit"
            );
            Ok(())
        },
    )?;
    // From here, errors mean the commit exists and index reconciliation is
    // needed; callers must inspect the intent instead of committing again.
    let mut final_index = &index_lease.file;
    index_lease
        .file
        .set_permissions(fs::Permissions::from_mode(
            fs::metadata(&index_path)?.mode(),
        ))?;
    final_index.write_all(&fs::read(&next_index)?)?;
    index_lease.file.sync_all()?;
    fs::rename(&index_lease.path, &index_path)
        .context("commit exists; the real index needs reconciliation")?;
    File::open(index_path.parent().context("index directory missing")?)?.sync_all()?;
    let mut warning = hook(repo, &index, "post-commit", &[])
        .err()
        .map(|e| format!("{e:#}"));
    if text(git(&repo.root, &["rev-parse", "HEAD"])?)? != commit {
        warning = Some("The reviewed commit was created, but a post-commit hook changed HEAD. Inspect history before continuing.".into());
    }
    Ok(CommitOutcome {
        commit,
        post_commit_warning: warning,
    })
}

/// Reconcile only a recorded, exact commit. Never rerun activation or hooks.
/// Returns None when a proven unpublished intent was archived and can be retried.
pub fn recover_commit(repo: &Repository, directory: &Path) -> Result<Option<CommitOutcome>> {
    let intent_path = directory.join("commit-intent.json");
    if !intent_path.exists() {
        return Ok(None);
    }
    let intent: CommitIntent = serde_json::from_slice(&fs::read(&intent_path)?)
        .context("commit intent cannot be reconciled automatically; inspect HEAD and index")?;
    ensure!(intent.schema_version == 1, "unsupported commit intent");
    let current = repo.fingerprint()?;
    let index_path = git_path(repo, "index")?;
    let lease_path = index_path.with_extension("lock");
    ensure!(
        intent.index_lock_anchor.parent() == index_path.parent()
            && intent
                .index_lock_anchor
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".nixos-updates-lease-")),
        "invalid recorded Git lock anchor"
    );
    let anchor_owned = || {
        fs::symlink_metadata(&intent.index_lock_anchor).is_ok_and(|meta| {
            meta.is_file()
                && meta.dev() == intent.index_lock_device
                && meta.ino() == intent.index_lock_inode
        })
    };
    let owned_lease = || -> Result<Option<Lease>> {
        match fs::symlink_metadata(&lease_path) {
            Ok(meta) => {
                ensure!(
                    anchor_owned()
                        && meta.is_file()
                        && meta.dev() == intent.index_lock_device
                        && meta.ino() == intent.index_lock_inode,
                    "another Git index lock exists; it was preserved"
                );
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&lease_path)?;
                let opened = file.metadata()?;
                ensure!(
                    opened.dev() == meta.dev() && opened.ino() == meta.ino(),
                    "Git lock changed during recovery"
                );
                Ok(Some(Lease {
                    path: lease_path.clone(),
                    file,
                    anchor: Some(intent.index_lock_anchor.clone()),
                }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    };
    if current == intent.expected {
        // The compare-and-swap did not publish. Keep an audit copy and allow retry.
        drop(owned_lease()?);
        let archive = tempfile::Builder::new()
            .prefix("commit-unpublished-")
            .tempfile_in(directory)?;
        fs::rename(&intent_path, archive.path())?;
        archive.keep()?;
        File::open(directory)?.sync_all()?;
        return Ok(None);
    }
    ensure!(
        current.head == intent.commit && current.branch == intent.expected.branch,
        "HEAD or branch changed after the commit intent; manual reconciliation required"
    );
    ensure!(
        current.source_hash == intent.expected.source_hash
            && current.lock_hash == intent.expected.lock_hash,
        "checkout changed after the commit intent; it was preserved"
    );
    let relative = repo.lock_relative.to_str().context("lock path encoding")?;
    ensure!(
        text(git(
            &repo.root,
            &["rev-parse", &format!("{}^{{tree}}", intent.commit)]
        )?)? == intent.tree
            && text(git(
                &repo.root,
                &["rev-parse", &format!("{}^", intent.commit)]
            )?)? == intent.expected.head,
        "recorded commit tree or parent does not match"
    );
    ensure!(
        git(
            &repo.root,
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-z",
                "-r",
                &intent.expected.head,
                &intent.commit
            ]
        )? == [relative.as_bytes(), b"\0"].concat()
            && text(git(
                &repo.root,
                &["rev-parse", &format!("{}:{relative}", intent.commit)]
            )?)? == intent.lock_blob
            && hash(&git(
                &repo.root,
                &["show", &format!("{}:{relative}", intent.commit)]
            )?) == intent.expected.lock_hash,
        "recorded commit is not the exact reviewed lock change"
    );
    let next_bytes = fs::read(directory.join("commit-next-index"))?;
    ensure!(
        hash(&next_bytes) == intent.next_index_hash,
        "recovery index bytes changed"
    );
    if current.index_hash != intent.next_index_hash {
        ensure!(
            current.index_hash == intent.expected.index_hash,
            "index changed externally; it was preserved"
        );
        let index_lease = match owned_lease()? {
            Some(lease) => lease,
            None => Lease::acquire(lease_path)?,
        };
        let _head_lease = Lease::acquire(git_path(repo, "HEAD")?.with_extension("lock"))?;
        ensure!(
            repo.fingerprint()? == current,
            "repository changed during commit recovery"
        );
        index_lease
            .file
            .set_permissions(fs::metadata(&index_path)?.permissions())?;
        index_lease.file.set_len(0)?;
        let mut file = &index_lease.file;
        file.write_all(&next_bytes)?;
        file.sync_all()?;
        fs::rename(&index_lease.path, &index_path)?;
        File::open(index_path.parent().context("index parent missing")?)?.sync_all()?;
    } else if anchor_owned()
        && fs::symlink_metadata(&lease_path).is_ok_and(|meta| {
            meta.dev() == intent.index_lock_device && meta.ino() == intent.index_lock_inode
        })
    {
        drop(owned_lease()?);
    }
    Ok(Some(CommitOutcome {
        commit: intent.commit,
        post_commit_warning: Some("Recovered the exact published commit and index. Post-commit hook completion is unknown; hooks were not rerun.".into()),
    }))
}
