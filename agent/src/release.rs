//! A *release* is a small directory of text files (at minimum `pixi.toml` and
//! `pixi.lock`) that fully describes what a robot should run.
//!
//! Releases are content-addressed: the release id is the SHA-256 of the files,
//! so any node can verify a bundle it received from any other node.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Component, Path};

/// Upper bound for one bundle. Keeps a misbehaving peer from filling the disk.
pub const MAX_BUNDLE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bundle {
    /// Human-friendly name. NOT part of the hash.
    #[serde(default)]
    pub label: Option<String>,
    /// Relative path -> file contents (UTF-8 text).
    pub files: BTreeMap<String, String>,
}

impl Bundle {
    /// SHA-256 over (path, contents) pairs in sorted path order.
    /// Every field is length-prefixed so `("ab","c")` and `("a","bc")` differ.
    pub fn hash(&self) -> String {
        let mut h = Sha256::new();
        for (path, content) in &self.files {
            h.update((path.len() as u64).to_be_bytes());
            h.update(path.as_bytes());
            h.update((content.len() as u64).to_be_bytes());
            h.update(content.as_bytes());
        }
        hex::encode(h.finalize())
    }

    /// Checks that the bundle is safe to write to disk and is a valid Pixi project.
    pub fn validate(&self) -> Result<()> {
        if !self.files.contains_key("pixi.toml") {
            bail!("bundle has no pixi.toml");
        }
        if !self.files.contains_key("pixi.lock") {
            bail!("bundle has no pixi.lock (releases must be fully locked)");
        }
        let mut total = 0usize;
        for (path, content) in &self.files {
            check_relative_path(path)?;
            total += path.len() + content.len();
        }
        if total > MAX_BUNDLE_BYTES {
            bail!("bundle too large ({total} bytes, limit {MAX_BUNDLE_BYTES})");
        }
        Ok(())
    }

    /// Packs a directory into a bundle. Skips `.pixi`, `.git` and other dot-directories.
    pub fn from_dir(dir: &Path, label: Option<String>) -> Result<Bundle> {
        let mut files = BTreeMap::new();
        walk(dir, dir, &mut files)?;
        let b = Bundle { label, files };
        b.validate()
            .with_context(|| format!("{} is not a valid release directory", dir.display()))?;
        Ok(b)
    }
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read {}", dir.display()))?
        .collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let ty = entry.file_type()?;
        if ty.is_dir() {
            if name.starts_with('.') {
                continue;
            }
            walk(root, &path, out)?;
        } else if ty.is_file() {
            let rel = path
                .strip_prefix(root)?
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let text = std::fs::read_to_string(&path).with_context(|| {
                format!(
                    "{} is not UTF-8 text (only text files are supported)",
                    path.display()
                )
            })?;
            out.insert(rel, text);
        }
    }
    Ok(())
}

fn check_relative_path(p: &str) -> Result<()> {
    if p.is_empty() || p.contains('\\') || p.contains('\0') {
        bail!("invalid file path in bundle: {p:?}");
    }
    for c in Path::new(p).components() {
        match c {
            Component::Normal(_) => {}
            _ => bail!("unsafe file path in bundle: {p:?}"),
        }
    }
    Ok(())
}

/// Release ids are exactly 64 lowercase hex characters. Checking this before
/// any hash is used in a file path rules out path traversal (`../..`).
pub fn is_valid_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn short(hash: &str) -> &str {
    &hash[..hash.len().min(8)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(files: &[(&str, &str)]) -> Bundle {
        Bundle {
            label: None,
            files: files
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn hash_is_stable_and_ignores_label() {
        let a = bundle(&[("pixi.toml", "a"), ("pixi.lock", "b")]);
        let mut b = a.clone();
        b.label = Some("v1".into());
        assert_eq!(a.hash(), b.hash());
        assert!(is_valid_hash(&a.hash()));
    }

    #[test]
    fn hash_changes_with_content_and_with_path_boundaries() {
        let a = bundle(&[("pixi.toml", "a"), ("pixi.lock", "b")]);
        let b = bundle(&[("pixi.toml", "a"), ("pixi.lock", "c")]);
        assert_ne!(a.hash(), b.hash());
        let c = bundle(&[("ab", "c")]);
        let d = bundle(&[("a", "bc")]);
        assert_ne!(c.hash(), d.hash());
    }

    #[test]
    fn validation_rejects_unsafe_paths_and_missing_files() {
        assert!(bundle(&[("pixi.toml", "x")]).validate().is_err());
        let ok = bundle(&[("pixi.toml", "x"), ("pixi.lock", "y")]);
        assert!(ok.validate().is_ok());
        let evil = bundle(&[("pixi.toml", "x"), ("pixi.lock", "y"), ("../escape", "z")]);
        assert!(evil.validate().is_err());
        let abs = bundle(&[("pixi.toml", "x"), ("pixi.lock", "y"), ("/etc/passwd", "z")]);
        assert!(abs.validate().is_err());
    }

    #[test]
    fn hash_validity_check() {
        assert!(!is_valid_hash("../../etc"));
        assert!(!is_valid_hash(&"A".repeat(64)));
        assert!(is_valid_hash(&"a".repeat(64)));
    }
}
