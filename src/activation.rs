//! The privileged boundary accepts only exact, existing NixOS store closures.
//! It never evaluates a flake, reads a user's checkout, or builds a system.
use crate::process::capture;
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const ACTIVATION_LOCK: &str = "/run/lock/nixos-updates-activation.lock";

pub fn activation_lease() -> Result<File> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "activation lease requires root"
    );
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(ACTIVATION_LOCK)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == 0 && meta.mode() & 0o022 == 0,
        "activation lock is not trusted"
    );
    file.set_permissions(fs::Permissions::from_mode(0o644))?;
    file.try_lock_exclusive()
        .context("another exact-system activation is running")?;
    Ok(file)
}

pub fn activation_is_running() -> Result<bool> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(ACTIVATION_LOCK)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == 0 && meta.mode() & 0o022 == 0,
        "activation lock is not trusted"
    );
    match FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatus {
    pub running: PathBuf,
    pub profile: PathBuf,
}

pub fn system_status() -> Result<SystemStatus> {
    Ok(SystemStatus {
        running: Path::new("/run/current-system")
            .canonicalize()
            .context("running system is unavailable")?,
        profile: Path::new("/nix/var/nix/profiles/system")
            .canonicalize()
            .context("system profile is unavailable")?,
    })
}

pub fn validate_store_path(path: &Path) -> Result<()> {
    ensure!(
        path.parent() == Some(Path::new("/nix/store")),
        "system must be a direct Nix store path"
    );
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("invalid store name")?;
    let (hash, name) = name.split_once('-').context("invalid store name")?;
    ensure!(
        hash.len() == 32
            && hash
                .bytes()
                .all(|b| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&b))
            && !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+-._?=".contains(&b)),
        "invalid Nix store path"
    );
    ensure!(
        path.canonicalize()? == path,
        "system path must not be a symlink"
    );
    Ok(())
}

pub fn validate_system(path: &Path) -> Result<()> {
    validate_store_path(path)?;
    let system = fs::symlink_metadata(path)?;
    ensure!(
        system.is_dir() && system.uid() == 0 && system.mode() & 0o022 == 0,
        "system closure must be an immutable root-owned directory"
    );
    let switch = path.join("bin/switch-to-configuration");
    let script = fs::metadata(&switch)?;
    ensure!(
        script.is_file()
            && script.uid() == 0
            && script.mode() & 0o022 == 0
            && script.mode() & 0o111 != 0,
        "system has no trusted activation script"
    );
    let resolved = switch.canonicalize()?;
    ensure!(
        resolved.starts_with("/nix/store"),
        "activation script escaped the Nix store"
    );
    ensure!(
        path.join("nixos-version").is_file(),
        "candidate is not a NixOS system closure"
    );
    Ok(())
}

/// Called only by the authenticated helper after its stdin handshake.
pub fn activate_exact(system: &Path, expected: &Path) -> Result<SystemStatus> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "activation requires the authenticated root helper"
    );
    validate_system(system)?;
    let before = system_status()?;
    ensure!(
        before.running == expected && before.profile == expected,
        "running system or profile changed since review; refresh before applying"
    );
    let nix_env =
        option_env!("NIXOS_UPDATES_NIX_ENV").unwrap_or("/run/current-system/sw/bin/nix-env");
    capture(
        Command::new(nix_env)
            .args(["-p", "/nix/var/nix/profiles/system", "--set"])
            .arg(system),
        Duration::from_secs(120),
    )
    .context("registering the exact system profile")?;
    capture(
        Command::new(system.join("bin/switch-to-configuration")).arg("switch"),
        Duration::from_secs(1200),
    )
    .context(
        "activation may have changed the profile or services; inspect actual state before retrying",
    )?;
    let after = system_status()?;
    ensure!(
        after.running == system && after.profile == system,
        "activation returned without the expected running system/profile; inspect actual state"
    );
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_paths_outside_the_direct_store_namespace() {
        for path in [
            "/tmp/system",
            "/nix/store/../system",
            "/nix/store/fake-system",
            "/nix/store/00000000000000000000000000000000-system/bin",
            "relative",
        ] {
            assert!(
                validate_store_path(Path::new(path)).is_err(),
                "accepted {path}"
            );
        }
    }
}
