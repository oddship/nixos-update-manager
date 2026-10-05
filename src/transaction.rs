//! Durable bookkeeping for the app's lock write. This does not activate a system.
use crate::repository::hash;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LockPhase {
    Prepared,
    WriteStarted,
    Written,
    Restored,
    Conflict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockTransaction {
    pub schema_version: u32,
    pub target: PathBuf,
    pub original_hash: String,
    pub candidate_hash: String,
    pub phase: LockPhase,
}

impl LockTransaction {
    pub fn create(
        directory: &Path,
        target: &Path,
        original: &[u8],
        candidate: &[u8],
    ) -> Result<Self> {
        ensure!(
            target.is_absolute() && target.file_name().is_some_and(|name| name == "flake.lock"),
            "transaction target must be an absolute flake.lock path"
        );
        ensure!(
            !directory.join("transaction.json").exists(),
            "a lock transaction already exists; recover it first"
        );
        ensure!(
            hash(&read_regular(target)?) == hash(original),
            "checkout lock changed before the transaction"
        );
        atomic_write(
            &directory.join("transaction-original.lock"),
            original,
            0o600,
        )?;
        atomic_write(
            &directory.join("transaction-candidate.lock"),
            candidate,
            0o600,
        )?;
        let record = Self {
            schema_version: 1,
            target: target.to_owned(),
            original_hash: hash(original),
            candidate_hash: hash(candidate),
            phase: LockPhase::Prepared,
        };
        record.save(directory)?;
        Ok(record)
    }

    pub fn load(directory: &Path) -> Result<Self> {
        let record: Self =
            serde_json::from_slice(&read_regular(&directory.join("transaction.json"))?)?;
        ensure!(
            record.schema_version == 1
                && record.target.is_absolute()
                && record
                    .target
                    .file_name()
                    .is_some_and(|name| name == "flake.lock"),
            "invalid lock transaction"
        );
        Ok(record)
    }

    fn save(&self, directory: &Path) -> Result<()> {
        atomic_write(
            &directory.join("transaction.json"),
            &serde_json::to_vec_pretty(self)?,
            0o600,
        )
    }

    pub fn write(&mut self, directory: &Path) -> Result<()> {
        ensure!(
            matches!(self.phase, LockPhase::Prepared),
            "lock transaction cannot be written twice"
        );
        let candidate = read_regular(&directory.join("transaction-candidate.lock"))?;
        ensure!(
            hash(&candidate) == self.candidate_hash,
            "candidate transaction bytes changed"
        );
        self.phase = LockPhase::WriteStarted;
        self.save(directory)?;
        if let Err(error) = replace_checked(&self.target, &self.original_hash, &candidate) {
            self.phase = LockPhase::Conflict;
            self.save(directory)?;
            return Err(error);
        }
        self.phase = LockPhase::Written;
        self.save(directory)
    }

    /// Inspect interrupted writes without assuming that a phase persisted after rename.
    pub fn reconcile(&mut self, directory: &Path) -> Result<()> {
        let current = hash(&read_regular(&self.target)?);
        self.phase = if current == self.original_hash {
            if self.phase == LockPhase::Prepared {
                LockPhase::Prepared
            } else {
                LockPhase::Restored
            }
        } else if current == self.candidate_hash
            && matches!(self.phase, LockPhase::WriteStarted | LockPhase::Written)
        {
            LockPhase::Written
        } else {
            LockPhase::Conflict
        };
        self.save(directory)
    }

    /// Restore only the expected app-written bytes. An external edit is retained.
    /// This restores a file, never services, the system profile, or a generation.
    pub fn restore(&mut self, directory: &Path) -> Result<bool> {
        self.reconcile(directory)?;
        if self.phase != LockPhase::Written {
            return Ok(false);
        }
        let original = read_regular(&directory.join("transaction-original.lock"))?;
        ensure!(
            hash(&original) == self.original_hash,
            "original transaction bytes changed"
        );
        if let Err(error) = replace_checked(&self.target, &self.candidate_hash, &original) {
            self.phase = LockPhase::Conflict;
            self.save(directory)?;
            return Err(error);
        }
        self.phase = LockPhase::Restored;
        self.save(directory)?;
        Ok(true)
    }
}

fn read_regular(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "transaction files must be ordinary files"
    );
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "transaction file exceeds the size limit"
    );
    Ok(bytes)
}

fn replace_checked(path: &Path, expected_hash: &str, bytes: &[u8]) -> Result<()> {
    let original = fs::symlink_metadata(path)?;
    ensure!(
        original.is_file() && original.uid() == unsafe { libc::geteuid() },
        "lock must be an ordinary file owned by this user"
    );
    ensure!(
        hash(&read_regular(path)?) == expected_hash,
        "lock changed externally; it was preserved"
    );
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().context("lock parent missing")?)?;
    temp.write_all(bytes)?;
    // Preserve ownership group and mode, including repositories with shared groups.
    ensure!(
        unsafe { libc::fchown(temp.as_file().as_raw_fd(), original.uid(), original.gid()) } == 0,
        "cannot preserve lock ownership"
    );
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(original.mode()))?;
    temp.as_file().sync_all()?;
    ensure!(
        hash(&read_regular(path)?) == expected_hash,
        "lock changed before replacement; it was preserved"
    );
    temp.persist(path)?;
    File::open(path.parent().context("lock parent missing")?)?.sync_all()?;
    ensure!(
        hash(&read_regular(path)?) == hash(bytes),
        "lock changed after replacement; inspect the recovery record"
    );
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("record parent missing")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        fs::create_dir(&state).unwrap();
        let target = temp.path().join("flake.lock");
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        (temp, state, target)
    }
    #[test]
    fn own_write_recovers_across_a_missing_completion_record_and_preserves_mode() {
        let (_temp, state, target) = fixture();
        let mut tx = LockTransaction::create(&state, &target, b"old", b"new").unwrap();
        tx.write(&state).unwrap();
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o640);
        tx.phase = LockPhase::WriteStarted;
        tx.save(&state).unwrap();
        let mut recovered = LockTransaction::load(&state).unwrap();
        recovered.reconcile(&state).unwrap();
        assert_eq!(recovered.phase, LockPhase::Written);
        assert!(recovered.restore(&state).unwrap());
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert!(!recovered.restore(&state).unwrap());
    }
    #[test]
    fn external_edits_before_write_or_after_write_are_never_restored_over() {
        let (_temp, state, target) = fixture();
        let mut tx = LockTransaction::create(&state, &target, b"old", b"new").unwrap();
        fs::write(&target, b"external-before").unwrap();
        assert!(tx.write(&state).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"external-before");
        assert!(!tx.restore(&state).unwrap());
        let (_temp2, state2, target2) = fixture();
        let mut tx = LockTransaction::create(&state2, &target2, b"old", b"new").unwrap();
        tx.write(&state2).unwrap();
        fs::write(&target2, b"external-after").unwrap();
        assert!(!tx.restore(&state2).unwrap());
        assert_eq!(tx.phase, LockPhase::Conflict);
        assert_eq!(fs::read(&target2).unwrap(), b"external-after");
    }
}
