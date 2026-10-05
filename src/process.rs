use anyhow::{Context, Result, bail};
use std::os::unix::process::CommandExt;
use std::{
    cell::RefCell,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

struct CommandGroup(Child);
impl Drop for CommandGroup {
    fn drop(&mut self) {
        // EOF aborts a prepared Git transaction cleanly, releasing its locks.
        self.0.stdin.take();
        for _ in 0..20 {
            if self.0.try_wait().ok().flatten().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let killed = unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL) };
        if killed == 0 {
            let _ = self.0.wait();
        }
    }
}

/// Supervise a two-phase command: wait for a readiness line, run the caller's
/// final validation, then send confirmation. EOF/errors abort prepared Git
/// transactions and unconfirmed authentication handshakes.
pub fn dialogue(
    command: &mut Command,
    timeout: Duration,
    initial_input: &[u8],
    ready_line: &str,
    confirmation: &[u8],
    authorize: impl FnOnce() -> Result<()>,
) -> Result<Vec<u8>> {
    use std::io::{BufRead, BufReader};
    cancellation_checkpoint()?;
    let mut output = tempfile::tempfile()?;
    let mut error = tempfile::tempfile()?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(error.try_clone()?)
        .process_group(0);
    let mut child = CommandGroup(command.spawn().context("starting two-phase command")?);
    let stdin = child.0.stdin.as_mut().context("command stdin missing")?;
    stdin.write_all(initial_input)?;
    stdin.flush()?;
    let stdout = child.0.stdout.take().context("command stdout missing")?;
    let mut writer = output.try_clone()?;
    let wanted = ready_line.to_owned();
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || -> Result<()> {
        let mut input = BufReader::new(stdout.take(16 * 1024 * 1024 + 1));
        let mut line = Vec::new();
        let mut announced = false;
        loop {
            line.clear();
            if input.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            writer.write_all(&line)?;
            if !announced && line == wanted.as_bytes() {
                announced = true;
                let _ = tx.send(());
            }
        }
        Ok(())
    });
    let started = Instant::now();
    let mut validation = Some(authorize);
    let status = loop {
        cancellation_checkpoint()?;
        if rx.try_recv().is_ok() {
            let validate = validation
                .take()
                .context("command announced readiness twice")?;
            validate()?;
            let mut stdin = child.0.stdin.take().context("command stdin missing")?;
            stdin.write_all(confirmation)?;
            stdin.flush()?;
        }
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if started.elapsed() > timeout {
            bail!("two-phase command timed out; inspect actual state before retrying");
        }
        if output.metadata()?.len() > 16 * 1024 * 1024 || error.metadata()?.len() > 16 * 1024 * 1024
        {
            bail!("two-phase command exceeded its output limit");
        }
        thread::sleep(Duration::from_millis(20));
    };
    // Stop any descendants still holding the stdout pipe before joining.
    drop(child);
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("command output reader failed"))??;
    if !status.success() {
        bail!(
            "two-phase command failed ({status}): {}",
            redact(&String::from_utf8_lossy(&read(&mut error)?))
        );
    }
    if validation.is_some() {
        bail!("command exited without the required readiness handshake");
    }
    if error.metadata()?.len() > 16 * 1024 * 1024 {
        bail!("command output exceeded the size limit");
    }
    read(&mut output)
}

#[derive(Debug)]
pub struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("operation cancelled; the checkout was not changed")
    }
}
impl std::error::Error for Cancelled {}

thread_local! {
    static CANCEL_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Cancellation is scoped to this worker thread, including repository commands.
pub struct CancellationScope(Option<PathBuf>);
impl CancellationScope {
    pub fn new(path: PathBuf) -> Self {
        Self(CANCEL_PATH.with(|slot| slot.replace(Some(path))))
    }
}
impl Drop for CancellationScope {
    fn drop(&mut self) {
        CANCEL_PATH.with(|slot| slot.replace(self.0.take()));
    }
}
pub fn cancellation_checkpoint() -> Result<()> {
    if CANCEL_PATH.with(|slot| slot.borrow().as_ref().is_some_and(|p| p.exists())) {
        return Err(Cancelled.into());
    }
    Ok(())
}

/// Capture to disk so a noisy child cannot deadlock on a full pipe. Every child
/// has its own process group, allowing a timeout to stop descendants too.
pub fn capture(command: &mut Command, timeout: Duration) -> Result<Vec<u8>> {
    capture_with_stdin(command, timeout, Stdio::null())
}

pub fn capture_input(command: &mut Command, timeout: Duration, input: &[u8]) -> Result<Vec<u8>> {
    if input.len() > 16 * 1024 * 1024 {
        bail!("command input exceeds the size limit");
    }
    let mut file = tempfile::tempfile()?;
    file.write_all(input)?;
    file.seek(SeekFrom::Start(0))?;
    capture_with_stdin(command, timeout, Stdio::from(file))
}

fn capture_with_stdin(command: &mut Command, timeout: Duration, stdin: Stdio) -> Result<Vec<u8>> {
    cancellation_checkpoint()?;
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    command
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?))
        .stdin(stdin)
        .process_group(0);
    let mut child = command.spawn().context("starting external command")?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let oversized = stdout.metadata()?.len() > 16 * 1024 * 1024
            || stderr.metadata()?.len() > 16 * 1024 * 1024;
        let cancelled = cancellation_checkpoint().is_err();
        if started.elapsed() > timeout || oversized || cancelled {
            // SAFETY: the child was spawned as the leader of its own process group.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGTERM);
            }
            thread::sleep(Duration::from_millis(100));
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            if cancelled {
                return Err(Cancelled.into());
            }
            if oversized {
                bail!(
                    "external command output exceeded the 16 MiB limit; its process group was stopped"
                );
            }
            bail!("command timed out; its process group was stopped");
        }
        thread::sleep(Duration::from_millis(20));
    };
    // A short-lived child can exceed the limit between polls and exit first.
    if stdout.metadata()?.len() > 16 * 1024 * 1024 || stderr.metadata()?.len() > 16 * 1024 * 1024 {
        bail!("external command output exceeded the 16 MiB limit");
    }
    if !status.success() {
        let error = read(&mut stderr)?;
        bail!(
            "command failed ({status}): {}",
            redact(&String::from_utf8_lossy(&error))
        );
    }
    read(&mut stdout)
}

fn read(file: &mut File) -> Result<Vec<u8>> {
    if file.metadata()?.len() > 16 * 1024 * 1024 {
        bail!("external command output exceeded the 16 MiB limit");
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn redact(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.contains("://") && (line.contains('@') || line.contains('?')) {
                "[URL-bearing diagnostic redacted]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_stops_descendants_and_does_not_leak_into_the_next_operation() {
        let temp = tempfile::tempdir().unwrap();
        let cancel = temp.path().join("cancel");
        let marker = temp.path().join("orphan-write");
        let writer_path = cancel.clone();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            std::fs::write(writer_path, "").unwrap();
        });
        let scope = CancellationScope::new(cancel);
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "(sleep 1; printf survived > \"$1\") & wait",
                "fixture",
            ])
            .arg(&marker);
        let error = capture(&mut command, Duration::from_secs(5)).unwrap_err();
        assert!(error.downcast_ref::<Cancelled>().is_some());
        writer.join().unwrap();
        drop(scope);
        assert_eq!(
            capture(Command::new("printf").arg("next"), Duration::from_secs(1)).unwrap(),
            b"next"
        );
        thread::sleep(Duration::from_millis(1200));
        assert!(!marker.exists(), "cancelled descendant survived");
    }
    #[test]
    fn timeout_stops_descendants_from_writing_after_the_parent_exits() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("orphan-write");
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "(sleep 1; printf survived > \"$1\") & wait",
                "fixture",
            ])
            .arg(&marker);
        assert!(capture(&mut command, Duration::from_millis(80)).is_err());
        thread::sleep(Duration::from_millis(1200));
        assert!(
            !marker.exists(),
            "a descendant survived the operation timeout"
        );
    }
}
