use std::{collections::BTreeSet, process::Command};

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageState {
    pub official: Vec<String>,
    pub aur: Vec<String>,
}

impl PackageState {
    pub fn total_unique(&self) -> usize {
        self.official
            .iter()
            .chain(&self.aur)
            .collect::<BTreeSet<_>>()
            .len()
    }
}

pub struct PackageBackend;

impl PackageBackend {
    pub fn current(&self) -> Result<PackageState> {
        Ok(PackageState {
            official: query_updates("checkupdates", &[])?,
            aur: query_updates("paru", &["-Qua"])?,
        })
    }
}

fn query_updates(program: &str, args: &[&str]) -> Result<Vec<String>> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to execute {program}"))?;
    // checkupdates and paru use exit status 2 to mean that no updates were found.
    if !output.status.success() && output.status.code() != Some(2) {
        bail!(
            "{program} exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8(output.stdout)
        .with_context(|| format!("{program} returned non-UTF-8 output"))?;
    parse_package_updates(&stdout)
}

pub fn parse_package_updates(output: &str) -> Result<Vec<String>> {
    let mut packages = BTreeSet::new();
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let name = line
            .split_whitespace()
            .next()
            .context("package update line had no package name")?;
        if name.starts_with('-') || name.contains('/') || name.contains('\\') {
            bail!("invalid package name in update output: {name}");
        }
        packages.insert(name.to_owned());
    }
    Ok(packages.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_deduplicates_update_lines() {
        assert_eq!(
            parse_package_updates("zlib 1.3 -> 1.4\nlinux 6.1 -> 6.2\nzlib 1.3 -> 1.4\n").unwrap(),
            vec!["linux", "zlib"]
        );
        assert!(parse_package_updates("\n").unwrap().is_empty());
    }

    #[test]
    fn rejects_suspicious_package_names() {
        assert!(parse_package_updates("--noconfirm 1 -> 2").is_err());
        assert!(parse_package_updates("repo/name 1 -> 2").is_err());
    }

    #[test]
    fn counts_overlap_only_once() {
        let state = PackageState {
            official: vec!["linux".into(), "shared".into()],
            aur: vec!["paru".into(), "shared".into()],
        };
        assert_eq!(state.total_unique(), 3);
    }
}
