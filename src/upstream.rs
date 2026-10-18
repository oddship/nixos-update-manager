//! Optional fixed-endpoint lookup, independent of model text or tool requests.
use crate::process::capture;
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{process::Command, time::Duration};

#[derive(Debug, Clone)]
pub struct HashQuery {
    package: String,
    expected: String,
    actual: String,
}

#[derive(Debug, Serialize)]
pub struct Evidence {
    pub source: String,
    pub finding: &'static str,
    pub limitation: &'static str,
}

fn sri(text: &str) -> Option<String> {
    let value = text
        .trim()
        .trim_matches('"')
        .trim_end_matches(';')
        .trim_matches('"');
    (value.starts_with("sha256-")
        && value.len() == 51
        && value[7..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)))
    .then(|| value.to_string())
}

pub fn hash_query(error: &str) -> Option<HashQuery> {
    // Only the actual hash-mismatch block can supply a query; downstream errors
    // and model output cannot choose endpoints, shell arguments, or source paths.
    let block = error
        .split_once("hash mismatch in fixed-output derivation '")?
        .1;
    let path = block.split_once('\'')?.0;
    let name = path
        .strip_prefix("/nix/store/")?
        .split_once('-')?
        .1
        .strip_suffix("-pnpm-deps.drv")?;
    let package = name
        .split('-')
        .take_while(|s| !s.starts_with(|c: char| c.is_ascii_digit()))
        .collect::<Vec<_>>()
        .join("-");
    if package.len() < 2
        || package.len() > 100
        || !package
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        return None;
    }
    let mut lines = block.lines().skip(1);
    let expected = sri(lines.next()?.trim().strip_prefix("specified:")?)?;
    let actual = sri(lines.next()?.trim().strip_prefix("got:")?)?;
    Some(HashQuery {
        package,
        expected,
        actual,
    })
}

impl HashQuery {
    pub fn url(&self) -> String {
        format!(
            "https://raw.githubusercontent.com/NixOS/nixpkgs/nixos-unstable/pkgs/by-name/{}/{}/package.nix",
            &self.package[..2],
            self.package
        )
    }

    pub fn lookup(&self) -> Result<Evidence> {
        let mut command = Command::new(option_env!("NIXOS_UPDATES_CURL").unwrap_or("curl"));
        command.args([
            "--disable",
            "--fail",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--connect-timeout",
            "10",
            "--max-time",
            "20",
            "--max-filesize",
            "131072",
            &self.url(),
        ]);
        let bytes = capture(&mut command, Duration::from_secs(25))
            .context("read Nixpkgs upstream definition")?;
        ensure!(bytes.len() <= 131072, "upstream definition is too large");
        let source = String::from_utf8(bytes)?;
        Ok(self.inspect(&source))
    }

    fn inspect(&self, source: &str) -> Evidence {
        let deps = source
            .split_once("pnpmDeps = fetchPnpmDeps {")
            .and_then(|(_, s)| s.split_once("};"))
            .map(|(s, _)| s);
        let declares = |hash: &str| {
            deps.is_some_and(|block| {
                block
                    .lines()
                    .any(|line| line.trim() == format!("hash = \"{hash}\";"))
            })
        };
        let finding = if declares(&self.actual) {
            "Current Nixpkgs nixos-unstable declares the observed dependency hash for this package. A newer input may address this mismatch."
        } else if declares(&self.expected) {
            "Current Nixpkgs nixos-unstable still declares the expected hash from the failed candidate. No correction was verified."
        } else {
            "The current definition does not establish a matching correction. The package layout, version, or dependency set may differ."
        };
        Evidence {
            source: self.url(),
            finding,
            limitation: "This checks one current upstream package definition, not the candidate's provenance or dependency contents. It does not verify version equivalence, channel availability, build success, or system compatibility. No files or hashes were changed.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn error() -> String {
        format!(
            "error: hash mismatch in fixed-output derivation '/nix/store/{}-example-language-server-pnpm-deps.drv':\n specified: sha256-{}=\n got: sha256-{}=",
            "a".repeat(32),
            "A".repeat(43),
            "B".repeat(43)
        )
    }
    #[test]
    fn endpoint_is_derived_only_from_validated_package_names() {
        let q = hash_query(&error()).unwrap();
        assert!(q.url().ends_with("/ex/example-language-server/package.nix"));
        assert!(hash_query(&error().replace("example-language-server", "../../evil")).is_none());
        assert!(hash_query("unfamiliar error").is_none());
    }
    #[test]
    fn a_hash_elsewhere_in_the_file_does_not_verify_dependency_correction() {
        let q = hash_query(&error()).unwrap();
        assert!(
            q.inspect(&format!("hash = \"{}\";", q.actual))
                .finding
                .contains("does not establish")
        );
        assert!(
            q.inspect(&format!(
                "pnpmDeps = fetchPnpmDeps {{\n hash = \"{}\";\n}};",
                q.actual
            ))
            .finding
            .contains("declares the observed")
        );
        assert!(
            q.inspect(&format!(
                "pnpmDeps = fetchPnpmDeps {{\n hash = \"{}\";\n}};",
                q.expected
            ))
            .finding
            .contains("No correction was verified")
        );
    }
}
