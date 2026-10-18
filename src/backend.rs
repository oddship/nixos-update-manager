use crate::{
    process::{CancellationScope, Cancelled, cancellation_checkpoint, capture},
    repository::{Fingerprint, Repository, hash},
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InputPolicy {
    pub update: Vec<String>,
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Checking,
    UpToDate,
    InputOnly,
    UpdatesFound,
    Preparing,
    Reviewing,
    Ready,
    ReviewStale,
    Stale,
    Failed,
    Cancelled,
    AwaitingAuthentication,
    Applying,
    Applied,
    ApplyNeedsAttention,
    Committing,
    Committed,
    CommitNeedsAttention,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputChange {
    pub name: String,
    pub current: Option<String>,
    pub next: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputPackage {
    pub input: String,
    pub attribute: String,
    #[serde(default)]
    pub version: String,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub schema_version: u32,
    pub id: String,
    pub repository: Repository,
    pub fingerprint: Fingerprint,
    pub host: String,
    pub policy: InputPolicy,
    pub created_at: u64,
    pub state: State,
    pub changed_inputs: Vec<String>,
    #[serde(default)]
    pub input_changes: Vec<InputChange>,
    #[serde(default)]
    pub input_packages: Vec<InputPackage>,
    #[serde(default)]
    pub installed_input_packages: Vec<InputPackage>,
    #[serde(default)]
    pub attribution_error: Option<String>,
    pub skipped_inputs: Vec<String>,
    pub current_drv: Option<String>,
    pub candidate_drv: Option<String>,
    pub candidate_lock_hash: Option<String>,
    pub prepared_system: Option<PathBuf>,
    pub review_baseline: Option<PathBuf>,
    pub closure_diff: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub created_at_millis: u64,
    #[serde(default)]
    pub application: Option<crate::apply::ApplicationRecord>,
}

#[derive(Debug)]
struct ReviewChanged;
impl std::fmt::Display for ReviewChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the running system changed; refresh the review before proceeding")
    }
}
impl std::error::Error for ReviewChanged {}

pub struct Store {
    pub root: PathBuf,
    _lock: File,
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self._lock);
    }
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        ensure!(
            !crate::activation::activation_is_running()?,
            "an authenticated activation is still running; wait for it to finish"
        );
        fs::create_dir_all(root)?;
        let meta = fs::symlink_metadata(root)?;
        ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "state directory must be a real directory"
        );
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join("operation.lock"))?;
        lock.try_lock_exclusive()
            .context("another updater operation is already running")?;
        let store = Self {
            root: root.canonicalize()?,
            _lock: lock,
        };
        store.reconcile()?;
        Ok(store)
    }

    pub fn load(&self, id: &str) -> Result<Candidate> {
        Self::read(&self.root, id)
    }

    /// Atomic state snapshots are readable while an operation holds the lock.
    pub fn read(root: &Path, id: &str) -> Result<Candidate> {
        use std::io::Read;
        let dir = candidate_directory(root, id)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(dir.join("state.json"))?;
        ensure!(
            file.metadata()?.is_file(),
            "candidate record must be a regular file"
        );
        let mut bytes = Vec::new();
        file.take(32 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 32 * 1024 * 1024,
            "candidate record exceeds the size limit"
        );
        let c: Candidate = serde_json::from_slice(&bytes)?;
        ensure!(
            c.schema_version == 1 && c.id == id,
            "unsupported or mismatched candidate record"
        );
        Ok(c)
    }

    pub fn records(root: &Path) -> Result<Vec<Candidate>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if let Some(id) = entry.file_name().to_str()
                && id.starts_with("candidate-")
                && let Ok(c) = Self::read(root, id)
            {
                records.push(c);
            }
        }
        records.sort_by(|a, b| {
            (
                a.created_at_millis.max(a.created_at.saturating_mul(1000)),
                &a.id,
            )
                .cmp(&(
                    b.created_at_millis.max(b.created_at.saturating_mul(1000)),
                    &b.id,
                ))
        });
        Ok(records)
    }

    pub fn latest_for(root: &Path, flake: &Path, host: &str) -> Option<Candidate> {
        Self::records(root).ok()?.into_iter().rev().find(|c| {
            c.repository.root.join(&c.repository.flake_dir) == host_flake(flake, host)
                && c.host == host
        })
    }

    pub fn directory(&self, id: &str) -> Result<PathBuf> {
        candidate_directory(&self.root, id)
    }

    /// Acquiring the exclusive lock proves no backend worker owns these states.
    /// Never infer failure merely from a closed frontend or a PID.
    fn reconcile(&self) -> Result<()> {
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !id.starts_with("candidate-") {
                continue;
            }
            let Ok(mut c) = self.load(&id) else { continue };
            if matches!(c.state, State::AwaitingAuthentication | State::Applying) {
                self.recover_application(&mut c)?;
                continue;
            }
            if matches!(
                c.state,
                State::Applied | State::Committing | State::Committed | State::CommitNeedsAttention
            ) && !c
                .application
                .as_ref()
                .is_some_and(|record| record.activation_completed)
            {
                c.state = State::ApplyNeedsAttention;
                c.error = Some("This record has no successful activation completion receipt. Inspect actual system/services and Git history; automatic commit is disabled.".into());
                self.save(&c)?;
                continue;
            }
            if matches!(c.state, State::Committing) {
                self.reconcile_commit(&mut c)?;
                continue;
            }
            if matches!(
                c.state,
                State::Checking | State::Preparing | State::Reviewing
            ) {
                let cancelled = c.operation_id.as_ref().is_some_and(|token| {
                    valid_token(token) && entry.path().join(format!("cancel-{token}")).exists()
                });
                c.state = if cancelled {
                    State::Cancelled
                } else {
                    State::Failed
                };
                c.error = Some(if cancelled {
                    "Cancellation was requested before the worker stopped. Check again, or retry preparation.".into()
                } else {
                    "The worker stopped before recording completion. Check again, or retry preparation; no activation was attempted.".into()
                });
                c.operation_id = None;
                self.save(&c)?;
            } else if matches!(c.state, State::Ready) && !review_is_current(&c) {
                c.state = State::ReviewStale;
                c.error = Some(ReviewChanged.to_string());
                self.save(&c)?;
            }
        }
        Ok(())
    }

    /// Write a token-specific request without acquiring the worker's lock.
    pub fn request_cancel(root: &Path, id: &str) -> Result<bool> {
        let dir = candidate_directory(root, id)?;
        let c = Self::read(root, id)?;
        if !matches!(
            c.state,
            State::Checking | State::Preparing | State::Reviewing
        ) {
            return Ok(false);
        }
        let token = c
            .operation_id
            .context("worker has no cancellation token; reopen the app to recover it")?;
        ensure!(valid_token(&token), "invalid operation token");
        let marker = dir.join(format!("cancel-{token}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(marker)
        {
            Ok(file) => {
                file.sync_all()?;
                File::open(dir)?.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        Ok(true)
    }

    fn begin_operation(&self, c: &mut Candidate) -> Result<CancellationScope> {
        let dir = self.directory(&c.id)?;
        let token_file = tempfile::Builder::new()
            .prefix("operation-")
            .tempfile_in(&dir)?;
        let token = token_file
            .path()
            .file_name()
            .context("operation token")?
            .to_str()
            .context("operation token encoding")?
            .to_owned();
        let marker = dir.join(format!("cancel-{token}"));
        // Reserve this name for the candidate's lifetime, including retries.
        token_file.keep()?;
        c.operation_id = Some(token);
        self.save(c)?;
        Ok(CancellationScope::new(marker))
    }

    fn finish_operation(&self, c: &mut Candidate, result: Result<()>) -> Result<()> {
        let result = result
            .and_then(|()| cancellation_checkpoint())
            .and_then(|()| {
                if matches!(c.state, State::Ready) && !review_is_current(c) {
                    return Err(ReviewChanged.into());
                }
                Ok(())
            });
        if let Err(error) = result {
            c.state = if error.downcast_ref::<Cancelled>().is_some() {
                State::Cancelled
            } else if error.downcast_ref::<ReviewChanged>().is_some() {
                State::ReviewStale
            } else {
                State::Failed
            };
            c.error = Some(format!("{error:#}"));
        }
        c.operation_id = None;
        self.save(c)
    }

    pub fn save(&self, c: &Candidate) -> Result<()> {
        let dir = self.directory(&c.id)?;
        let mut temp = tempfile::NamedTempFile::new_in(&dir)?;
        use std::io::Write;
        temp.write_all(&serde_json::to_vec_pretty(c)?)?;
        temp.as_file().sync_all()?;
        temp.persist(dir.join("state.json"))?;
        File::open(dir)?.sync_all()?;
        Ok(())
    }

    pub fn check(&self, path: &Path, host: &str, policy: InputPolicy) -> Result<Candidate> {
        ensure!(!host.is_empty(), "choose a host configuration");
        let repository = Repository::open(&host_flake(path, host))?;
        ensure!(
            !self.root.starts_with(&repository.root),
            "keep updater state outside the configuration Git repository"
        );
        let fingerprint = repository.fingerprint()?;
        let dir = tempfile::Builder::new()
            .prefix("candidate-")
            .tempdir_in(&self.root)?
            .keep();
        let id = dir
            .file_name()
            .context("candidate path")?
            .to_str()
            .context("candidate name")?
            .to_owned();
        let mut c = Candidate {
            schema_version: 1,
            id,
            repository,
            fingerprint,
            host: host.into(),
            policy,
            created_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
            state: State::Checking,
            changed_inputs: vec![],
            input_changes: vec![],
            input_packages: vec![],
            installed_input_packages: vec![],
            attribution_error: None,
            skipped_inputs: vec![],
            current_drv: None,
            candidate_drv: None,
            candidate_lock_hash: None,
            prepared_system: None,
            review_baseline: None,
            closure_diff: None,
            error: None,
            operation_id: None,
            created_at_millis: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64,
            application: None,
        };
        let _scope = self.begin_operation(&mut c)?;
        let result = self.resolve(&mut c);
        self.finish_operation(&mut c, result)?;
        Ok(c)
    }

    fn resolve(&self, c: &mut Candidate) -> Result<()> {
        let dir = self.directory(&c.id)?;
        let flake = c.repository.snapshot(&dir.join("source"), &c.fingerprint)?;
        let old_lock = fs::read(flake.join("flake.lock"))?;
        fs::write(dir.join("original.lock"), &old_lock)?;
        let old: Value = serde_json::from_slice(&old_lock)?;
        validate_lock(&old, &c.repository.flake_dir)?;
        let hosts = discover_hosts(&flake)?;
        ensure!(
            hosts.iter().any(|h| h == &c.host),
            "selected host does not exist; available hosts: {}",
            hosts.join(", ")
        );
        let root = old["root"].as_str().context("lock root missing")?;
        let inputs = old["nodes"][root]["inputs"]
            .as_object()
            .context("invalid root inputs")?;
        for input in c.policy.update.iter().chain(&c.policy.exclude) {
            ensure!(
                inputs.contains_key(input),
                "input policy names an unknown input: {input}"
            );
        }
        let selected: Vec<_> = inputs
            .iter()
            .filter(|(name, value)| {
                value.is_string()
                    && (c.policy.update.is_empty() || c.policy.update.contains(name))
                    && !c.policy.exclude.contains(name)
            })
            .map(|(name, _)| name.clone())
            .collect();
        c.skipped_inputs = inputs
            .keys()
            .filter(|n| !selected.contains(n))
            .cloned()
            .collect();
        if selected.is_empty() {
            c.state = State::UpToDate;
            return self.require_fresh(c);
        }
        let mut command = nix();
        command
            .args(["flake", "update"])
            .args(&selected)
            .arg("--flake")
            .arg(&flake)
            .args(["--option", "allow-import-from-derivation", "false"]);
        capture(&mut command, Duration::from_secs(900))
            .context("resolving selected input updates")?;
        let new_lock = fs::read(flake.join("flake.lock"))?;
        let new: Value = serde_json::from_slice(&new_lock)?;
        validate_lock(&new, &c.repository.flake_dir)?;
        c.changed_inputs = changed_inputs(&old, &new)?;
        c.input_changes = input_changes(&old, &new, &c.changed_inputs)?;
        for excluded in &c.policy.exclude {
            let old_ref = resolve_reference(&old, &inputs[excluded], 0)?;
            let new_root = new["root"]
                .as_str()
                .context("candidate lock root missing")?;
            let new_ref = resolve_reference(&new, &new["nodes"][new_root]["inputs"][excluded], 0)?;
            ensure!(
                signature(&old, &json!(old_ref), 0)? == signature(&new, &json!(new_ref), 0)?,
                "updating selected inputs also advances excluded input {excluded}; choose a policy without shared conflicting nodes"
            );
        }
        c.candidate_lock_hash = Some(hash(&new_lock));
        fs::write(dir.join("candidate.lock"), &new_lock)?;
        if old == new {
            c.state = State::UpToDate;
            return self.require_fresh(c);
        }
        // Both sides are immutable to the user and faithfully expose their own lock.
        fs::write(flake.join("flake.lock"), &old_lock)?;
        c.current_drv = Some(evaluate_drv(&flake, &c.host)?);
        retain_derivation(
            c.current_drv
                .as_deref()
                .context("baseline derivation missing")?,
            &dir.join("baseline-drv-root"),
        )?;
        fs::write(flake.join("flake.lock"), &new_lock)?;
        c.candidate_drv = Some(evaluate_drv(&flake, &c.host)?);
        retain_derivation(
            c.candidate_drv
                .as_deref()
                .context("candidate derivation missing")?,
            &dir.join("candidate-drv-root"),
        )?;
        match input_packages(
            &dir.join("source"),
            &c.repository.flake_dir,
            &c.host,
            &c.changed_inputs,
        ) {
            Ok(packages) => c.input_packages = packages,
            Err(error) if error.downcast_ref::<Cancelled>().is_some() => return Err(error),
            Err(error) => c.attribution_error = Some(format!("{error:#}")),
        }
        c.state = if c.current_drv == c.candidate_drv {
            State::InputOnly
        } else {
            State::UpdatesFound
        };
        self.require_fresh(c)
    }

    pub fn require_fresh(&self, c: &Candidate) -> Result<()> {
        self.require_source_fresh(c)?;
        if matches!(c.state, State::Ready | State::ReviewStale) && !review_is_current(c) {
            return Err(ReviewChanged.into());
        }
        Ok(())
    }

    fn require_source_fresh(&self, c: &Candidate) -> Result<()> {
        ensure!(
            c.repository.fingerprint()? == c.fingerprint,
            "candidate is stale: repository, HEAD, branch, lock, or index changed; check again"
        );
        Ok(())
    }

    /// Realise the recorded derivation, never re-resolve/re-evaluate the live checkout.
    /// This unprivileged operation has no activation route.
    pub fn prepare(&self, id: &str) -> Result<Candidate> {
        let mut c = self.load(id)?;
        if let Err(error) = self.require_source_fresh(&c) {
            c.state = State::Stale;
            c.error = Some(error.to_string());
            self.save(&c)?;
            return Ok(c);
        }
        ensure!(
            matches!(
                c.state,
                State::UpdatesFound | State::InputOnly | State::Failed | State::Cancelled
            ),
            "candidate has no update to prepare"
        );
        let drv = c
            .candidate_drv
            .clone()
            .context("candidate has no evaluated derivation")?;
        c.state = State::Preparing;
        c.error = None;
        c.prepared_system = None;
        c.installed_input_packages.clear();
        c.review_baseline = None;
        c.closure_diff = None;
        let _scope = self.begin_operation(&mut c)?;
        let result = (|| -> Result<()> {
            let dir = self.directory(id)?;
            let mut cmd = nix();
            cmd.args(["build", "--json", "--out-link"])
                .arg(dir.join("gc-root"))
                .arg(format!("{drv}^out"));
            let built: Value =
                serde_json::from_slice(&capture(&mut cmd, Duration::from_secs(7200))?)?;
            let system = built[0]["outputs"]["out"]
                .as_str()
                .context("build returned no system output")?;
            c.prepared_system = Some(PathBuf::from(system));
            let (baseline, diff) = runtime_review(Path::new(system))?;
            c.closure_diff = Some(diff);
            c.review_baseline = Some(baseline);
            c.installed_input_packages.clear();
            if !c.input_packages.is_empty() {
                match installed_exports(Path::new(system), &c.input_packages) {
                    Ok(packages) => c.installed_input_packages = packages,
                    Err(error) if error.downcast_ref::<Cancelled>().is_some() => return Err(error),
                    Err(error) => c.attribution_error = Some(format!("{error:#}")),
                }
            }
            self.require_source_fresh(&c)?;
            c.state = State::Ready;
            Ok(())
        })();
        self.finish_operation(&mut c, result)?;
        Ok(c)
    }

    /// Refresh the runtime comparison of the retained output, without building.
    pub fn review(&self, id: &str) -> Result<Candidate> {
        let mut c = self.load(id)?;
        if let Err(error) = self.require_source_fresh(&c) {
            c.state = State::Stale;
            c.error = Some(error.to_string());
            self.save(&c)?;
            return Ok(c);
        }
        ensure!(
            matches!(
                c.state,
                State::Ready | State::ReviewStale | State::Failed | State::Cancelled
            ),
            "candidate cannot be reviewed in its current state"
        );
        let system = c
            .prepared_system
            .clone()
            .context("prepare the candidate before reviewing it")?;
        ensure!(
            self.directory(id)?.join("gc-root").canonicalize()? == system,
            "the retained output does not match the candidate; prepare again"
        );
        c.state = State::Reviewing;
        c.error = None;
        c.closure_diff = None;
        let _scope = self.begin_operation(&mut c)?;
        let result = (|| {
            let (baseline, diff) = runtime_review(&system)?;
            self.require_source_fresh(&c)?;
            c.review_baseline = Some(baseline);
            c.closure_diff = Some(diff);
            c.state = State::Ready;
            Ok(())
        })();
        self.finish_operation(&mut c, result)?;
        Ok(c)
    }
}

fn nix_string(value: &str) -> Result<String> {
    Ok(serde_json::to_string(value)?.replace("${", "\\${"))
}

fn input_packages(
    root: &Path,
    flake_dir: &Path,
    host: &str,
    inputs: &[String],
) -> Result<Vec<InputPackage>> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    // A Git reference keeps relative path inputs resolvable against the whole snapshot.
    let mut reference = format!("git+file://{}", root.display());
    if !flake_dir.as_os_str().is_empty() {
        reference.push_str(&format!("?dir={}", flake_dir.display()));
    }
    let source = nix_string(&reference)?;
    let host = nix_string(host)?;
    let inputs = inputs
        .iter()
        .map(|input| nix_string(input))
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    let expression = format!(
        "let flake = builtins.getFlake {source}; system = flake.nixosConfigurations.{host}.pkgs.stdenv.hostPlatform.system; in ({}) {{ flake = {source}; inputs = [ {inputs} ]; inherit system; }}",
        include_str!("input-packages.nix")
    );
    let mut cmd = nix();
    cmd.args(["eval", "--impure", "--json", "--expr"])
        .arg(expression)
        .args(["--option", "allow-import-from-derivation", "false"]);
    serde_json::from_slice(&capture(&mut cmd, Duration::from_secs(30))?)
        .context("input package provenance unavailable")
}

fn installed_exports(system: &Path, packages: &[InputPackage]) -> Result<Vec<InputPackage>> {
    let mut cmd = nix();
    cmd.args(["path-info", "--recursive"]).arg(system);
    let paths = String::from_utf8(capture(&mut cmd, Duration::from_secs(30))?)?;
    let installed: std::collections::HashSet<_> = paths.lines().map(Path::new).collect();
    Ok(packages
        .iter()
        .filter(|package| installed.contains(package.path.as_path()))
        .cloned()
        .collect())
}

fn review_is_current(c: &Candidate) -> bool {
    c.review_baseline.as_ref().is_some_and(|baseline| {
        Path::new("/run/current-system")
            .canonicalize()
            .ok()
            .as_ref()
            == Some(baseline)
    })
}

fn runtime_review(system: &Path) -> Result<(PathBuf, String)> {
    collect_review(
        system,
        Path::new("/run/current-system"),
        |baseline, system| {
            let mut cmd = nix();
            cmd.env("NO_COLOR", "1")
                .env("TERM", "dumb")
                .args(["store", "diff-closures"])
                .arg(baseline)
                .arg(system);
            Ok(String::from_utf8(capture(
                &mut cmd,
                Duration::from_secs(120),
            )?)?)
        },
    )
}

fn collect_review(
    system: &Path,
    running: &Path,
    diff: impl FnOnce(&Path, &Path) -> Result<String>,
) -> Result<(PathBuf, String)> {
    let baseline = running
        .canonicalize()
        .context("running system baseline is unavailable")?;
    let changes = diff(&baseline, system)?;
    if running.canonicalize()? != baseline {
        return Err(ReviewChanged.into());
    }
    Ok((baseline, changes))
}

fn valid_token(token: &str) -> bool {
    token.starts_with("operation-")
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn candidate_directory(root: &Path, id: &str) -> Result<PathBuf> {
    ensure!(
        id.starts_with("candidate-") && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "invalid candidate identifier"
    );
    let dir = root.join(id);
    if let Ok(meta) = fs::symlink_metadata(&dir) {
        ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "candidate must be a real directory"
        );
    }
    Ok(dir)
}

fn nix() -> Command {
    let mut cmd = Command::new("nix");
    let cores = std::env::var("NIXOS_UPDATES_BUILD_CORES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|cores| *cores > 0)
        .unwrap_or(2);
    cmd.args([
        "--extra-experimental-features",
        "nix-command flakes",
        "--max-jobs",
        "1",
        "--cores",
        &cores.to_string(),
    ]);
    cmd
}

fn validate_lock(lock: &Value, flake_dir: &Path) -> Result<()> {
    ensure!(
        lock["version"] == json!(7),
        "only flake lock schema 7 is validated"
    );
    for id in lock["nodes"]
        .as_object()
        .context("invalid lock nodes")?
        .keys()
    {
        local_path_input(lock, id, flake_dir, 0)?;
    }
    Ok(())
}

/// Relative path inputs resolve inside the Git snapshot; absolute or escaping paths would
/// read live sources outside it. Returns the repository-relative location of local inputs.
fn local_path_input(
    lock: &Value,
    id: &str,
    flake_dir: &Path,
    depth: usize,
) -> Result<Option<PathBuf>> {
    ensure!(depth < 64, "path input chain is too deep");
    let node = &lock["nodes"][id];
    if node["locked"]["type"] != "path" {
        return Ok(None);
    }
    let path = Path::new(
        node["locked"]["path"]
            .as_str()
            .context("path input has no path")?,
    );
    let parent = node["parent"].as_array().with_context(|| {
        format!(
            "path input {id} must be relative to its flake; absolute path inputs are not supported"
        )
    })?;
    ensure!(
        path.is_relative(),
        "path input {id} must be relative to its flake; absolute path inputs are not supported"
    );
    let base = if parent.is_empty() {
        flake_dir.to_owned()
    } else {
        let parent_id = resolve_reference(lock, &node["parent"], depth + 1)?;
        match local_path_input(lock, parent_id, flake_dir, depth + 1)? {
            Some(base) => base,
            None => return Ok(None),
        }
    };
    let mut location = PathBuf::new();
    for component in base.join(path).components() {
        match component {
            std::path::Component::Normal(part) => location.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => ensure!(
                location.pop(),
                "path input {id} points outside the Git repository"
            ),
            _ => anyhow::bail!("path input {id} points outside the Git repository"),
        }
    }
    Ok(Some(location))
}

/// A folder with `hosts/<name>/flake.nix` keeps one independently locked flake per host.
pub fn host_flakes(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder.join("hosts"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().join("flake.nix").is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

pub fn host_flake(folder: &Path, host: &str) -> PathBuf {
    let plain = matches!(
        Path::new(host).components().collect::<Vec<_>>()[..],
        [std::path::Component::Normal(_)]
    );
    let candidate = folder.join("hosts").join(host);
    if plain && candidate.join("flake.nix").is_file() {
        candidate
    } else {
        folder.to_owned()
    }
}

pub fn discover_hosts(flake: &Path) -> Result<Vec<String>> {
    let mut cmd = nix();
    cmd.args(["eval", "--json"])
        .arg(format!("{}#nixosConfigurations", flake.display()))
        .args([
            "--apply",
            "builtins.attrNames",
            "--no-write-lock-file",
            "--no-update-lock-file",
            "--option",
            "allow-import-from-derivation",
            "false",
        ]);
    Ok(serde_json::from_slice(&capture(
        &mut cmd,
        Duration::from_secs(300),
    )?)?)
}

fn evaluate_drv(flake: &Path, host: &str) -> Result<String> {
    let mut cmd = nix();
    let host = nix_string(host)?;
    cmd.args(["eval", "--raw"])
        .arg(format!(
            "{}#nixosConfigurations.{host}.config.system.build.toplevel.drvPath",
            flake.display()
        ))
        .args([
            "--no-write-lock-file",
            "--no-update-lock-file",
            "--option",
            "allow-import-from-derivation",
            "false",
        ]);
    let drv = String::from_utf8(capture(&mut cmd, Duration::from_secs(900))?)?;
    ensure!(
        drv.starts_with("/nix/store/") && drv.ends_with(".drv"),
        "evaluation returned an invalid system derivation"
    );
    Ok(drv)
}

fn retain_derivation(drv: &str, root: &Path) -> Result<()> {
    // A storePath expression evaluates to the derivation as a plain store object,
    // unlike `drv^out`, and creates an indirect GC root without realising outputs.
    let mut cmd = nix();
    cmd.args(["build", "--impure", "--expr"])
        .arg(format!(
            "builtins.storePath {}",
            serde_json::to_string(drv)?
        ))
        .arg("--out-link")
        .arg(root);
    capture(&mut cmd, Duration::from_secs(60))?;
    Ok(())
}

fn resolve_reference<'a>(lock: &'a Value, reference: &'a Value, depth: usize) -> Result<&'a str> {
    ensure!(
        depth < 64,
        "input graph contains a cycle or exceeds the supported depth"
    );
    if let Some(node) = reference.as_str() {
        return Ok(node);
    }
    let path = reference.as_array().context("invalid input reference")?;
    let mut node = lock["root"].as_str().context("lock root missing")?;
    for segment in path {
        let input = segment.as_str().context("invalid follows path")?;
        node = resolve_reference(lock, &lock["nodes"][node]["inputs"][input], depth + 1)?;
    }
    Ok(node)
}

fn signature(lock: &Value, reference: &Value, depth: usize) -> Result<Value> {
    ensure!(depth < 64, "input dependency graph is too deep");
    let id = resolve_reference(lock, reference, depth)?;
    let node = &lock["nodes"][id];
    let mut children = serde_json::Map::new();
    if let Some(inputs) = node["inputs"].as_object() {
        for (name, child) in inputs {
            children.insert(name.clone(), signature(lock, child, depth + 1)?);
        }
    }
    Ok(json!({"locked":node["locked"],"original":node["original"],"inputs":children}))
}

fn input_changes(old: &Value, new: &Value, names: &[String]) -> Result<Vec<InputChange>> {
    fn revision(lock: &Value, name: &str) -> Result<Option<String>> {
        let root = lock["root"].as_str().context("lock root missing")?;
        let reference = &lock["nodes"][root]["inputs"][name];
        if reference.is_null() {
            return Ok(None);
        }
        let node = resolve_reference(lock, reference, 0)?;
        let locked = &lock["nodes"][node]["locked"];
        Ok(locked["rev"]
            .as_str()
            .or_else(|| locked["narHash"].as_str())
            .map(str::to_owned))
    }
    names
        .iter()
        .map(|name| {
            Ok(InputChange {
                name: name.clone(),
                current: revision(old, name)?,
                next: revision(new, name)?,
            })
        })
        .collect()
}

fn changed_inputs(old: &Value, new: &Value) -> Result<Vec<String>> {
    let old_root = old["root"].as_str().context("original lock root missing")?;
    let new_root = new["root"]
        .as_str()
        .context("candidate lock root missing")?;
    let old_inputs = old["nodes"][old_root]["inputs"]
        .as_object()
        .context("invalid original inputs")?;
    let new_inputs = new["nodes"][new_root]["inputs"]
        .as_object()
        .context("invalid candidate inputs")?;
    let names: std::collections::BTreeSet<_> = old_inputs.keys().chain(new_inputs.keys()).collect();
    names
        .into_iter()
        .filter_map(|name| match (old_inputs.get(name), new_inputs.get(name)) {
            (Some(a), Some(b)) => match (signature(old, a, 0), signature(new, b, 0)) {
                (Ok(a), Ok(b)) => (a != b).then(|| Ok(name.clone())),
                (Err(e), _) | (_, Err(e)) => Some(Err(e)),
            },
            _ => Some(Ok(name.clone())),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_review_rejects_a_generation_change_during_comparison() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old");
        let new = temp.path().join("new");
        fs::create_dir(&old).unwrap();
        fs::create_dir(&new).unwrap();
        let running = temp.path().join("running");
        symlink(&old, &running).unwrap();
        let stable = collect_review(&new, &running, |baseline, system| {
            assert_eq!(baseline, old);
            assert_eq!(system, new);
            Ok("review".into())
        })
        .unwrap();
        assert_eq!(stable, (old.clone(), "review".into()));
        let error = collect_review(&new, &running, |_, _| {
            fs::remove_file(&running)?;
            symlink(&new, &running)?;
            Ok("obsolete review".into())
        })
        .unwrap_err();
        assert!(error.downcast_ref::<ReviewChanged>().is_some());
    }
    #[test]
    fn per_host_flakes_are_resolved_from_the_chosen_folder() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path();
        for host in ["rynix", "zephy"] {
            fs::create_dir_all(folder.join("hosts").join(host)).unwrap();
            fs::write(folder.join("hosts").join(host).join("flake.nix"), "{}").unwrap();
        }
        fs::create_dir_all(folder.join("hosts/notes")).unwrap();
        assert_eq!(host_flakes(folder), vec!["rynix", "zephy"]);
        assert_eq!(host_flake(folder, "zephy"), folder.join("hosts/zephy"));
        assert_eq!(host_flake(folder, "notes"), folder);
        assert_eq!(host_flake(folder, "../hosts/zephy"), folder);
        assert!(host_flakes(&folder.join("hosts/zephy")).is_empty());
    }
    #[test]
    fn path_inputs_must_stay_inside_the_repository() {
        let lock = |path: &str, parent: Value| {
            json!({"version":7,"root":"root","nodes":{
                "root":{"inputs":{"common":"common","dep":"dep"}},
                "common":{"locked":{"type":"path","path":path},"parent":parent,"inputs":{"nested":"nested"}},
                "dep":{"locked":{"type":"github","rev":"one"},"inputs":{"inner":"inner"}},
                "inner":{"locked":{"type":"path","path":"../../../x"},"parent":["dep"]},
                "nested":{"locked":{"type":"path","path":"./sub"},"parent":["common"]}
            }})
        };
        let host = Path::new("machines/hosts/zephy");
        validate_lock(&lock("../../common", json!([])), host).unwrap();
        assert_eq!(
            local_path_input(&lock("../../common", json!([])), "nested", host, 0).unwrap(),
            Some(PathBuf::from("machines/common/sub"))
        );
        assert!(validate_lock(&lock("../../../../outside", json!([])), host).is_err());
        assert!(validate_lock(&lock("/etc/nixos", json!([])), host).is_err());
        assert!(validate_lock(&lock("../../common", Value::Null), host).is_err());
    }
    #[test]
    fn changed_inputs_includes_follows_and_transitive_dependency_changes() {
        let old = json!({"root":"root","nodes":{
            "root":{"inputs":{"base":"base","alias":["base"],"wrapper":"wrapper"}},
            "base":{"locked":{"rev":"one"}},
            "wrapper":{"locked":{"rev":"same"},"inputs":{"base":["base"]}}
        }});
        let mut new = old.clone();
        new["nodes"]["base"]["locked"]["rev"] = json!("two");
        assert_eq!(
            changed_inputs(&old, &new).unwrap(),
            vec!["alias", "base", "wrapper"]
        );
        let changes = input_changes(&old, &new, &changed_inputs(&old, &new).unwrap()).unwrap();
        assert_eq!(changes[0].current.as_deref(), Some("one"));
        assert_eq!(changes[0].next.as_deref(), Some("two"));
        assert_eq!(changes[2].current, changes[2].next);
        let mut hash_lock = old.clone();
        hash_lock["nodes"]["base"]["locked"] = json!({"narHash": "sha256-content"});
        let changes = input_changes(&old, &hash_lock, &["alias".into()]).unwrap();
        assert_eq!(changes[0].next.as_deref(), Some("sha256-content"));
    }
}
