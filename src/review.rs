//! Presentation data derived from Nix's recorded closure review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageChange {
    pub name: String,
    pub description: String,
    pub next_versions: Vec<String>,
}

// Preserve Nix's version sets: unchanged versions may also be installed.
// Unknown output remains available in the raw review, never guessed here.
pub fn package_changes(output: &str) -> Vec<PackageChange> {
    output
        .lines()
        .filter_map(|line| {
            let clean = strip_ansi(line);
            let (name, change) = clean.split_once(": ")?;
            if name.is_empty() || change.is_empty() {
                return None;
            }
            let versions = change
                .rsplit_once(", ")
                .filter(|(_, tail)| {
                    (tail.starts_with('+') || tail.starts_with('-'))
                        && [" KiB", " MiB", " GiB", " B"]
                            .iter()
                            .any(|unit| tail.ends_with(unit))
                })
                .map_or(change, |(versions, _)| versions);
            let next_versions = versions
                .split_once(" → ")
                .map(|(old, new)| {
                    let old_versions: Vec<_> = old.split(", ").collect();
                    new.split(", ")
                        .filter(|v| *v != "∅" && !old_versions.contains(v))
                        .map(|v| {
                            if v == "ε" {
                                String::new()
                            } else {
                                v.to_owned()
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            let description = if let Some((old, new)) = versions.split_once(" → ") {
                let label = |v: &str| if v == "ε" { "unversioned" } else { v }.to_owned();
                match (old, new) {
                    ("∅", new) => format!("Added · {}", label(new)),
                    (old, "∅") => format!("Removed · {}", label(old)),
                    (old, new) => format!("{} → {}", label(old), label(new)),
                }
            } else if (change.starts_with('+') || change.starts_with('-')) && change.ends_with("B")
            {
                format!("Size changed · {change}")
            } else {
                return None;
            };
            Some(PackageChange {
                name: name.to_owned(),
                description,
                next_versions,
            })
        })
        .collect()
}

pub fn group_packages(
    changes: Vec<PackageChange>,
    exports: &[crate::backend::InputPackage],
) -> (
    std::collections::BTreeMap<String, Vec<PackageChange>>,
    Vec<PackageChange>,
) {
    let mut groups = std::collections::BTreeMap::<String, Vec<PackageChange>>::new();
    let mut other = Vec::new();
    for change in changes {
        let inputs: std::collections::BTreeSet<_> = exports
            .iter()
            .filter(|export| {
                export.name == change.name && change.next_versions.contains(&export.version)
            })
            .map(|export| &export.input)
            .collect();
        if inputs.len() == 1 {
            groups
                .entry((*inputs.first().unwrap()).clone())
                .or_default()
                .push(change);
        } else {
            other.push(change);
        }
    }
    (groups, other)
}

fn strip_ansi(text: &str) -> String {
    let mut chars = text.chars();
    let mut clean = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.next() == Some('[') {
                for code in chars.by_ref() {
                    if ('@'..='~').contains(&code) {
                        break;
                    }
                }
            }
        } else {
            clean.push(ch);
        }
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grouping_keeps_ambiguous_and_unmapped_packages_separate_and_deduplicates_aliases() {
        use crate::backend::InputPackage;
        let export = |input: &str, name: &str| InputPackage {
            input: input.into(),
            attribute: "default".into(),
            version: "2".into(),
            name: name.into(),
            path: "/nix/store/example".into(),
        };
        let exports = [
            export("tools", "app"),
            export("tools", "app"),
            export("first", "shared"),
            export("second", "shared"),
            export("tools", "retained"),
        ];
        let (groups, other) = group_packages(
            package_changes("app: 1 → 2\nshared: 1 → 2\nunknown: 1 → 2\nretained: 2 → 2, 3"),
            &exports,
        );
        assert_eq!(groups["tools"].len(), 1);
        assert_eq!(
            other
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["shared", "unknown", "retained"]
        );
    }
    #[test]
    fn closure_review_preserves_version_sets_and_handles_add_remove_and_sizes() {
        let rows = package_changes(
            "app: 1.2, 1.3 → 1.4, +13.9 KiB\nnew: ∅ → 2.0\nold: 1.0 → ∅, -1.2 MiB\nconfig: ε → ∅\nrebuilt: \x1b[32;1m+12.6 KiB\x1b[0m\nunknown output\n",
        );
        assert_eq!(
            rows.iter()
                .map(|r| r.description.as_str())
                .collect::<Vec<_>>(),
            [
                "1.2, 1.3 → 1.4",
                "Added · 2.0",
                "Removed · 1.0",
                "Removed · unversioned",
                "Size changed · +12.6 KiB"
            ]
        );
        assert_eq!(rows[0].name, "app");
        assert!(package_changes("").is_empty());
        assert!(package_changes("app: undocumented output").is_empty());
    }
}
