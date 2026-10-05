use crate::process::capture;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fingerprint {
    pub head: String,
    pub branch: String,
    pub source_hash: String,
    pub index_hash: String,
    pub lock_hash: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub root: PathBuf,
    pub flake_dir: PathBuf,
    pub lock_relative: PathBuf,
}

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0");
    // Caller-supplied Git index/worktree overrides must not redirect repository operations.
    for name in [
        "GIT_INDEX_FILE",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
    ] {
        cmd.env_remove(name);
    }
    capture(&mut cmd, Duration::from_secs(60))
}

fn string(bytes: Vec<u8>) -> Result<String> {
    Ok(String::from_utf8(bytes)?.trim().into())
}

impl Repository {
    pub fn open(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .context("configuration folder does not exist")?;
        ensure!(
            path.join("flake.nix").is_file(),
            "choose a folder containing flake.nix"
        );
        let root = PathBuf::from(string(git(&path, &["rev-parse", "--show-toplevel"])?)?);
        ensure!(
            string(git(&root, &["rev-parse", "--is-shallow-repository"])?)? == "false",
            "shallow repositories are not supported yet"
        );
        let flake_dir = path.strip_prefix(&root)?.to_owned();
        let lock_relative = flake_dir.join("flake.lock");
        let repo = Self {
            root,
            flake_dir,
            lock_relative,
        };
        let entries = repo.entries()?;
        ensure!(
            entries.iter().any(|(_, name)| name == &repo.lock_relative),
            "flake.lock must exist and be tracked by Git; create and review it before setup"
        );
        ensure!(
            entries
                .iter()
                .any(|(_, name)| name == &repo.flake_dir.join("flake.nix")),
            "flake.nix must be tracked by Git"
        );
        repo.fingerprint()?;
        Ok(repo)
    }

    fn entries(&self) -> Result<Vec<(String, PathBuf)>> {
        let bytes = git(&self.root, &["ls-files", "--stage", "-z"])?;
        let mut entries = Vec::new();
        for record in bytes.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
            let record = std::str::from_utf8(record)
                .context("non-UTF-8 Git filenames are not supported yet")?;
            let (meta, name) = record.split_once('\t').context("invalid Git index entry")?;
            let fields: Vec<_> = meta.split_whitespace().collect();
            ensure!(
                fields.len() == 3 && fields[2] == "0",
                "resolve Git merge conflicts before checking"
            );
            ensure!(
                matches!(fields[0], "100644" | "100755"),
                "symlinks and Git submodules are not supported yet: {name}"
            );
            let path = PathBuf::from(name);
            ensure!(
                !path.is_absolute()
                    && !path
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir)),
                "unsafe Git path"
            );
            entries.push((meta.to_owned(), path));
        }
        Ok(entries)
    }

    pub fn fingerprint(&self) -> Result<Fingerprint> {
        self.fingerprint_with_lock(None)
    }

    fn fingerprint_with_lock(&self, original: Option<&[u8]>) -> Result<Fingerprint> {
        let mut digest = Sha256::new();
        for (_, path) in self.entries()? {
            let source = self.root.join(&path);
            let mut ancestor = source.parent();
            while let Some(parent) = ancestor {
                if parent == self.root {
                    break;
                }
                match fs::symlink_metadata(parent) {
                    Ok(meta) => ensure!(
                        !meta.file_type().is_symlink(),
                        "tracked parent directories must not be symlinks: {}",
                        parent.display()
                    ),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                ancestor = parent.parent();
            }
            digest.update((path.as_os_str().len() as u64).to_le_bytes());
            digest.update(path.as_os_str().as_encoded_bytes());
            match fs::symlink_metadata(&source) {
                Ok(meta) => {
                    ensure!(
                        meta.is_file(),
                        "tracked paths must remain ordinary files: {}",
                        path.display()
                    );
                    let bytes = match original.filter(|_| path == self.lock_relative) {
                        Some(bytes) => bytes.to_vec(),
                        None => fs::read(source)?,
                    };
                    digest.update((bytes.len() as u64).to_le_bytes());
                    digest.update(meta.permissions().mode().to_le_bytes());
                    digest.update(bytes);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => digest.update(b"deleted"),
                Err(e) => return Err(e.into()),
            }
        }
        let index_path = string(git(
            &self.root,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )?)?;
        Ok(Fingerprint {
            head: string(git(&self.root, &["rev-parse", "HEAD"])?)?,
            branch: string(git(&self.root, &["symbolic-ref", "-q", "HEAD"])?)?,
            source_hash: format!("{:x}", digest.finalize()),
            index_hash: hash(&fs::read(index_path)?),
            lock_hash: hash(&fs::read(self.root.join(&self.lock_relative))?),
            status: String::from_utf8(git(
                &self.root,
                &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            )?)?,
        })
    }

    /// Validate the checkout after the app's only authorized source mutation.
    pub fn require_only_lock_change(
        &self,
        expected: &Fingerprint,
        original: &[u8],
        candidate_hash: &str,
    ) -> Result<Fingerprint> {
        let current = self.fingerprint()?;
        ensure!(
            current.lock_hash == candidate_hash,
            "checkout lock changed after the app's write"
        );
        let normalized = self.fingerprint_with_lock(Some(original))?;
        let without_lock = |status: &str| -> String {
            status
                .split('\0')
                .filter(|line| line.get(3..) != self.lock_relative.to_str())
                .collect::<Vec<_>>()
                .join("\0")
        };
        ensure!(
            normalized.head == expected.head
                && normalized.branch == expected.branch
                && normalized.index_hash == expected.index_hash
                && normalized.source_hash == expected.source_hash
                && without_lock(&normalized.status) == without_lock(&expected.status),
            "checkout, branch, HEAD, or index changed after review; activation stopped"
        );
        Ok(current)
    }

    /// Recreate HEAD/branch and the index, then copy only Git-tracked working bytes.
    /// Ignored/untracked contents and repository-local credentials are never copied.
    pub fn snapshot(&self, destination: &Path, expected: &Fingerprint) -> Result<PathBuf> {
        ensure!(
            &self.fingerprint()? == expected,
            "repository changed before snapshot; check again"
        );
        let mut clone = Command::new("git");
        clone
            .args(["clone", "--no-hardlinks", "--no-checkout", "--quiet", "--"])
            .arg(&self.root)
            .arg(destination);
        capture(&mut clone, Duration::from_secs(60))?;
        ensure!(
            string(git(destination, &["rev-parse", "HEAD"])?)? == expected.head,
            "HEAD changed while cloning; check again"
        );
        git(destination, &["remote", "remove", "origin"])?;
        git(destination, &["read-tree", "--empty"])?;
        for (meta, path) in self.entries()? {
            let fields: Vec<_> = meta.split_whitespace().collect();
            git(
                destination,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    fields[0],
                    fields[1],
                    path.to_str().context("invalid path")?,
                ],
            )?;
            let source = self.root.join(&path);
            if source.exists() {
                let target = destination.join(&path);
                fs::create_dir_all(target.parent().context("path has no parent")?)?;
                fs::copy(&source, target)?;
            }
        }
        ensure!(
            &self.fingerprint()? == expected,
            "repository changed while snapshotting; check again"
        );
        Ok(destination.join(&self.flake_dir))
    }
}
