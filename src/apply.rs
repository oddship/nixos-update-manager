use crate::{
    activation::{SystemStatus, system_status, validate_system},
    backend::{Candidate, State, Store},
    commit::{commit_lock, recover_commit},
    process::dialogue,
    repository::{Fingerprint, git, hash},
    transaction::{LockPhase, LockTransaction},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationRecord {
    pub directory: PathBuf,
    pub acknowledged: bool,
    #[serde(default)]
    pub activation_completed: bool,
    pub fingerprint: Option<Fingerprint>,
    pub actual: Option<SystemStatus>,
    pub commit: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApplyPlan {
    pub system: PathBuf,
    pub baseline: PathBuf,
    pub helper: PathBuf,
    pub original_lock_hash: String,
    pub candidate_lock_hash: String,
    pub can_commit: bool,
    pub commit_message: String,
    pub includes_local_edits: bool,
}

impl Store {
    pub fn apply_plan(&self, id: &str) -> Result<ApplyPlan> {
        let c = self.load(id)?;
        ensure!(
            matches!(c.state, State::Ready),
            "prepare and refresh the candidate review before applying"
        );
        self.require_fresh(&c)?;
        let system = c
            .prepared_system
            .context("candidate has no prepared system")?;
        validate_system(&system)?;
        ensure!(
            self.directory(id)?.join("gc-root").canonicalize()? == system,
            "retained output does not match the prepared system"
        );
        let baseline = c
            .review_baseline
            .context("candidate has no review baseline")?;
        let actual = system_status()?;
        ensure!(
            actual.running == baseline && actual.profile == baseline,
            "running system and profile must match the reviewed baseline"
        );
        let helper = PathBuf::from(
            option_env!("NIXOS_UPDATES_HELPER")
                // The installed package keeps this alias for older modules.
                .unwrap_or("/run/current-system/sw/bin/nixos-updates-helper"),
        )
        .canonicalize()
        .context("install the NixOS Update Manager system module to enable authenticated Apply")?;
        let meta = fs::metadata(&helper)?;
        ensure!(
            helper.starts_with("/nix/store")
                && meta.is_file()
                && meta.uid() == 0
                && meta.mode() & 0o022 == 0
                && meta.mode() & 0o111 != 0,
            "authenticated helper must be a trusted Nix store executable"
        );
        ensure!(
            Path::new("/run/wrappers/bin/pkexec").is_file(),
            "the NixOS module must enable the pkexec wrapper"
        );
        let relative = c
            .repository
            .lock_relative
            .to_str()
            .context("lock path encoding")?;
        let can_commit = git(&c.repository.root, &["show", &format!("HEAD:{relative}")])
            .is_ok_and(|bytes| hash(&bytes) == c.fingerprint.lock_hash)
            && git(&c.repository.root, &["show", &format!(":{relative}")])
                .is_ok_and(|bytes| hash(&bytes) == c.fingerprint.lock_hash);
        let new_hash = c
            .candidate_lock_hash
            .context("candidate has no lock change")?;
        ensure!(
            hash(&fs::read(self.directory(id)?.join("candidate.lock"))?) == new_hash,
            "candidate lock bytes changed"
        );
        Ok(ApplyPlan {
            system,
            baseline,
            helper,
            original_lock_hash: c.fingerprint.lock_hash,
            candidate_lock_hash: new_hash,
            can_commit,
            commit_message: format!("flake: update {}", c.changed_inputs.join(", ")),
            includes_local_edits: c
                .fingerprint
                .status
                .split('\0')
                .any(|entry| !entry.is_empty() && !entry.starts_with("?? ")),
        })
    }

    pub fn apply(&self, id: &str, commit: bool, message: Option<String>) -> Result<Candidate> {
        ensure!(
            !commit || !message.as_ref().is_some_and(|text| text.trim().is_empty()),
            "commit message is empty; choose Apply without a commit or enter a message"
        );
        let plan = self.apply_plan(id)?;
        ensure!(
            !commit || plan.can_commit,
            "automatic commit is disabled for a pre-existing dirty or staged lock"
        );
        let mut c = self.load(id)?;
        let directory = tempfile::Builder::new()
            .prefix("apply-")
            .tempdir_in(self.directory(id)?)?
            .keep();
        let original = fs::read(self.directory(id)?.join("original.lock"))?;
        let candidate = fs::read(self.directory(id)?.join("candidate.lock"))?;
        let mut tx = LockTransaction::create(
            &directory,
            &c.repository.root.join(&c.repository.lock_relative),
            &original,
            &candidate,
        )?;
        c.state = State::AwaitingAuthentication;
        c.error = None;
        c.application = Some(ApplicationRecord {
            directory: directory.clone(),
            acknowledged: false,
            activation_completed: false,
            fingerprint: None,
            actual: None,
            commit: None,
            message: commit.then(|| message.unwrap_or(plan.commit_message)),
        });
        self.save(&c)?;
        let mut cmd = Command::new("/run/wrappers/bin/pkexec");
        cmd.arg("--disable-internal-agent")
            .arg(plan.helper)
            .arg("--system")
            .arg(&plan.system)
            .arg("--expected-running")
            .arg(&plan.baseline);
        let result = dialogue(
            &mut cmd,
            Duration::from_secs(1800),
            b"",
            "{\"phase\":\"authenticated\",\"schema_version\":1}\n",
            b"activate\n",
            || {
                self.require_fresh(&c)?;
                let actual = system_status()?;
                ensure!(
                    actual.running == plan.baseline && actual.profile == plan.baseline,
                    "running system or profile changed during authentication"
                );
                c.state = State::Applying;
                self.save(&c)?;
                tx.write(&directory)?;
                let fp = c.repository.require_only_lock_change(
                    &c.fingerprint,
                    &original,
                    &plan.candidate_lock_hash,
                )?;
                let application = c
                    .application
                    .as_mut()
                    .context("application record missing")?;
                application.fingerprint = Some(fp);
                application.acknowledged = true;
                self.save(&c)?;
                Ok(())
            },
        );
        if let Err(error) = result {
            c.error = Some(format!("{error:#}"));
            self.recover_application(&mut c)?;
            return Ok(c);
        }
        c.application
            .as_mut()
            .context("application record missing")?
            .activation_completed = true;
        self.save(&c)?;
        self.recover_application(&mut c)?;
        if matches!(c.state, State::Applied) && commit {
            return self.commit(id);
        }
        Ok(c)
    }

    pub(crate) fn recover_application(&self, c: &mut Candidate) -> Result<()> {
        if crate::activation::activation_is_running()? {
            c.state = State::Applying;
            return self.save(c);
        }
        let root = self.directory(&c.id)?;
        let application = c
            .application
            .as_mut()
            .context("interrupted application has no recovery record")?;
        ensure!(
            application.directory.parent() == Some(root.as_path()),
            "invalid application recovery path"
        );
        let actual = system_status().ok();
        let applied = actual.as_ref().is_some_and(|status| {
            Some(&status.running) == c.prepared_system.as_ref()
                && Some(&status.profile) == c.prepared_system.as_ref()
        });
        application.actual = actual;
        if applied && application.acknowledged && !application.activation_completed {
            c.state = State::ApplyNeedsAttention;
            c.error.get_or_insert_with(|| "The system links match the candidate, but successful activation completion was not recorded. Inspect services and the activation result before continuing; automatic commit is disabled.".into());
        } else if applied && application.acknowledged {
            c.state = State::Applied;
            if c.repository.fingerprint().ok().as_ref() != application.fingerprint.as_ref() {
                c.state = State::ApplyNeedsAttention;
                c.error = Some("The exact system is applied, but the checkout changed during activation. Automatic commit is disabled; inspect the checkout.".into());
            } else {
                c.error = None;
            }
        } else if application.acknowledged {
            c.state = State::ApplyNeedsAttention;
            c.error.get_or_insert_with(|| "Activation was started but the exact running system/profile could not be verified. The lock and recovery record were retained; inspect actual system state.".into());
        } else {
            let mut tx = LockTransaction::load(&application.directory)?;
            let restored = tx.restore(&application.directory)?;
            c.state = State::Failed;
            if matches!(tx.phase, LockPhase::Conflict) {
                c.state = State::ApplyNeedsAttention;
                c.error = Some("Activation was not confirmed, but the checkout lock changed externally. It was preserved; inspect the recovery record.".into());
            } else {
                c.error.get_or_insert_with(|| if restored { "The worker stopped before activation confirmation. Only the app's lock write was restored.".into() } else { "Authentication or pre-activation validation did not complete. No activation was confirmed.".into() });
            }
        }
        self.save(c)
    }

    pub(crate) fn reconcile_commit(&self, c: &mut Candidate) -> Result<()> {
        let record = c
            .application
            .as_ref()
            .context("commit application record missing")?;
        ensure!(
            record.directory.parent() == Some(self.directory(&c.id)?.as_path()),
            "invalid commit recovery path"
        );
        match recover_commit(&c.repository, &record.directory) {
            Ok(Some(outcome)) => {
                c.application
                    .as_mut()
                    .context("application record missing")?
                    .commit = Some(outcome.commit);
                c.state = State::Committed;
                c.error = outcome.post_commit_warning;
            }
            Ok(None) => {
                c.state = State::CommitNeedsAttention;
                c.error = Some("Commit worker stopped before publishing. Retry commit when ready; the system is not reapplied.".into());
            }
            Err(error) => {
                c.state = State::CommitNeedsAttention;
                c.error = Some(format!("{error:#}"));
            }
        }
        self.save(c)
    }

    pub fn commit(&self, id: &str) -> Result<Candidate> {
        let mut c = self.load(id)?;
        ensure!(
            matches!(c.state, State::Applied | State::CommitNeedsAttention),
            "commit retry requires a verified application; it never reapplies the system"
        );
        let record = c
            .application
            .clone()
            .context("application record missing")?;
        ensure!(
            record.activation_completed,
            "successful activation completion is not recorded; inspect the system and services before committing"
        );
        if record.directory.join("commit-intent.json").exists() {
            match recover_commit(&c.repository, &record.directory) {
                Ok(Some(outcome)) => {
                    c.application
                        .as_mut()
                        .context("application record missing")?
                        .commit = Some(outcome.commit);
                    c.state = State::Committed;
                    c.error = outcome.post_commit_warning;
                    self.save(&c)?;
                    return Ok(c);
                }
                Ok(None) => {}
                Err(error) => {
                    c.state = State::CommitNeedsAttention;
                    c.error = Some(format!("{error:#}"));
                    self.save(&c)?;
                    return Ok(c);
                }
            }
        }
        let expected = record
            .fingerprint
            .context("application has no verified checkout fingerprint")?;
        let message = record
            .message
            .context("no commit was requested; use Git manually")?;
        c.state = State::Committing;
        c.error = None;
        self.save(&c)?;
        match commit_lock(
            &c.repository,
            &expected,
            &c.fingerprint.lock_hash,
            c.candidate_lock_hash
                .as_deref()
                .context("candidate lock missing")?,
            &message,
            &record.directory,
        ) {
            Ok(outcome) => {
                c.application
                    .as_mut()
                    .context("application record missing")?
                    .commit = Some(outcome.commit);
                c.state = State::Committed;
                c.error = outcome.post_commit_warning;
            }
            Err(error) => {
                c.state = State::CommitNeedsAttention;
                c.error = Some(format!("{error:#}"));
            }
        }
        self.save(&c)?;
        Ok(c)
    }
}
