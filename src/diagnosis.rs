//! Local failure triage. Model input is a previewable, bounded copy, never a command.
use crate::backend::{Candidate, State};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Diagnosis {
    pub category: &'static str,
    pub summary: &'static str,
    pub next_step: &'static str,
    pub system_status: &'static str,
    pub diagnostic_excerpt: String,
    pub excerpt_truncated: bool,
}

pub fn available(c: &Candidate) -> bool {
    c.error.is_some()
        && matches!(
            c.state,
            State::Failed
                | State::Stale
                | State::ReviewStale
                | State::ApplyNeedsAttention
                | State::CommitNeedsAttention
                | State::Committed
        )
}

pub fn diagnose(c: &Candidate) -> Diagnosis {
    let error = c.error.as_deref().unwrap_or("");
    let text = error.to_ascii_lowercase();
    let (category, summary, next_step) = if text.contains("hash mismatch") {
        (
            "hash_mismatch",
            "A downloaded dependency does not match its expected hash.",
            "Check for newer inputs and prepare a new candidate. Retrying this candidate retains the same expected hash. Do not replace a hash without verifying the source.",
        )
    } else if text.contains("no space left") || text.contains("disk quota exceeded") {
        (
            "disk_space",
            "The operation ran out of storage.",
            "Inspect free space and Nix store usage. Review what can be removed before cleaning up, then retry.",
        )
    } else if text.contains("could not resolve")
        || text.contains("couldn't resolve")
        || text.contains("network is unreachable")
        || text.contains("connection timed out")
        || text.contains("unable to download")
        || text.contains("http error")
    {
        (
            "network",
            "A network request failed.",
            "Check connectivity and the failing service, then retry. A network failure alone does not mean the package definition is broken.",
        )
    } else if text.contains("request dismissed")
        || text.contains("not authorized")
        || text.contains("authentication")
        || text.contains("polkit")
        || text.contains("pkexec")
    {
        (
            "authentication",
            "Authentication or permission validation did not complete.",
            "Inspect the authentication error and installed helper/module configuration before retrying Apply.",
        )
    } else if text.contains("stale")
        || text.contains("checkout changed")
        || text.contains("repository changed")
        || text.contains("running system changed")
    {
        (
            "stale_review",
            "The checkout or running system changed after the candidate was checked.",
            "Check again after source changes, or refresh the retained review if only the running system changed.",
        )
    } else if text.contains("signing")
        || text.contains("gpg")
        || text.contains("hook")
        || matches!(c.state, State::CommitNeedsAttention)
    {
        (
            "git_commit",
            "The Git commit did not complete cleanly.",
            "Inspect Git signing, hooks, and checkout state. Use Retry commit when available; do not reapply the system to retry a commit.",
        )
    } else if text.contains("syntax error")
        || text.contains("does not exist")
        || text.contains("undefined variable")
        || text.contains("infinite recursion")
        || text.contains("assertion")
        || text.contains("allow-import-from-derivation")
    {
        (
            "evaluation",
            "Nix could not evaluate the selected configuration or its inputs.",
            "Inspect the first evaluation error and its source location. Fix the configuration or select compatible inputs, then check again.",
        )
    } else if text.contains("builder for")
        || text.contains("cannot build")
        || text.contains("build failed")
    {
        (
            "build",
            "A package build failed and blocked the candidate.",
            "Find the first failing derivation in Technical details. Dependent package failures usually describe the same underlying problem.",
        )
    } else {
        (
            "other",
            "The operation did not complete successfully.",
            "Review Technical details and the operation state. The optional model can explain unfamiliar diagnostics, but its suggestions need verification.",
        )
    };
    let system_status = match c.state {
        State::ApplyNeedsAttention => {
            "Activation may have changed services or the system profile. Inspect actual system state; rollback and success are not confirmed."
        }
        State::CommitNeedsAttention | State::Committed => {
            "The system was applied; the remaining issue concerns recording the Git commit. Do not apply again just to retry the commit."
        }
        State::Failed if c.application.is_some() => {
            "An Apply attempt was recorded. Inspect the activation receipt and actual system state before retrying; this diagnosis cannot establish rollback."
        }
        _ => "This record contains no Apply attempt. Check and Prepare do not activate the system.",
    };
    let (diagnostic_excerpt, excerpt_truncated) = excerpt(error);
    Diagnosis {
        category,
        summary,
        next_step,
        system_status,
        diagnostic_excerpt,
        excerpt_truncated,
    }
}

/// Best-effort redaction, deliberately conservative. The user reviews the exact
/// result before sharing: arbitrary build output can contain unrecognised secrets.
pub fn excerpt(error: &str) -> (String, bool) {
    let mut lines = Vec::new();
    let mut private_block = false;
    for line in error.lines() {
        let clean: String = line
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .collect();
        let lower = clean.to_ascii_lowercase();
        if lower.contains("-----begin") {
            private_block = true;
        }
        let sensitive = private_block
            || [
                "token",
                "password",
                "passwd",
                "secret",
                "authorization",
                "credential",
                "api_key",
                "api-key",
                "apikey",
                "cookie",
                "bearer ",
                "ssh-ed25519",
                "ssh-rsa",
            ]
            .iter()
            .any(|s| lower.contains(s));
        if lower.contains("-----end") {
            private_block = false;
        }
        if sensitive {
            lines.push("[sensitive diagnostic line omitted]".to_string());
            continue;
        }
        if clean.starts_with("copying path ")
            || clean.starts_with("building '")
            || clean.trim_start().starts_with("/nix/store/")
        {
            continue;
        }
        let safe = clean
            .split_whitespace()
            .map(|word| {
                if word.contains("://") || word.contains('@') {
                    return "[address omitted]".to_owned();
                }
                // Preserve derivation names while removing store hashes and local paths.
                if let Some((_, store)) = word.split_once("/nix/store/") {
                    if let Some((hash, name)) = store.split_once('-')
                        && hash.len() == 32
                        && hash.bytes().all(|b| b.is_ascii_alphanumeric())
                    {
                        return format!("[store]/{}", name);
                    }
                    return "[store path omitted]".into();
                }
                if word.contains('/')
                    || word.contains('\\')
                    || word.contains('=') && !word.starts_with("sha256-")
                {
                    return "[path or assignment omitted]".into();
                }
                word.to_owned()
            })
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(safe);
    }
    // Keep the first causal error even when a long dependency cascade follows it.
    // Nix often prints a large plan before the root failure and dependants after.
    let text = lines.join("\n");
    const LIMIT: usize = 8000;
    let chars: Vec<_> = text.chars().collect();
    let truncated = chars.len() > LIMIT;
    if !truncated {
        (text, false)
    } else {
        let cause_byte = text
            .lines()
            .scan(0, |offset, line| {
                let start = *offset;
                *offset += line.len() + 1;
                Some((start, line))
            })
            .find(|(_, line)| {
                line.starts_with("error:")
                    || line.contains("hash mismatch")
                    || line.contains("builder for")
            })
            .map(|(offset, _)| offset)
            .unwrap_or(0);
        let cause = text[..cause_byte].chars().count().saturating_sub(250);
        let end = (cause + 2000).min(chars.len());
        (
            format!(
                "{}\n[diagnostics shortened; first error context]\n{}\n[diagnostics shortened; final context]\n{}",
                chars[..500].iter().collect::<String>(),
                chars[cause..end].iter().collect::<String>(),
                chars[chars.len() - 5500..].iter().collect::<String>()
            ),
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redacts_paths_credentials_urls_and_private_keys() {
        let (text, _) = excerpt(&format!(
            "error: /home/alice/private/flake.nix failed\nTOKEN=very-private\nfetch https://user:pass@host/a?token=private\nAuthorization: abc\n-----BEGIN {kind}-----\nprivate-key-material\n-----END {kind}-----\nerror: unfamiliar failure",
            kind = "PRIVATE KEY"
        ));
        for secret in [
            "alice",
            "very-private",
            "user:pass",
            "private-key-material",
            "abc",
        ] {
            assert!(!text.contains(secret), "{text}");
        }
        assert!(text.contains("unfamiliar failure"));
    }
    #[test]
    fn long_dependency_cascades_do_not_hide_the_first_failure() {
        let log = format!(
            "{}\nerror: root hash mismatch\n{}\nerror: final dependent failed",
            "verbose progress\n".repeat(1000),
            "error: dependent failed\n".repeat(1000)
        );
        let (text, truncated) = excerpt(&log);
        assert!(truncated);
        assert!(text.contains("root hash mismatch"));
        assert!(text.ends_with("error: final dependent failed"));
        assert!(text.chars().count() < 8100);
    }
    #[test]
    fn long_unicode_logs_preserve_tail_and_bound_output() {
        let (text, truncated) = excerpt(&format!("{}\nerror: root cause", "é".repeat(30_000)));
        assert!(truncated);
        assert!(text.chars().count() < 8100);
        assert!(text.ends_with("error: root cause"));
    }
}
