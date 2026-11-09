//! On-disk layout managed by the agent:
//!
//! ```text
//! <data-dir>/
//!   bundles/<hash>.json     content-addressed release definitions (what peers exchange)
//!   releases/<hash>/        a release unpacked and installed by Pixi
//!   current  -> releases/<hash>   the active release (a symlink)
//!   previous -> releases/<hash>   the one before it, kept for fast rollback
//!   logs/app.log            output of the supervised robot application
//! ```
//!
//! A new release is installed next to the old one. Only after it passes its
//! health check does `current` flip, with a single `rename(2)`, which is atomic
//! on Linux: the robot is always on exactly one complete release.

use crate::release::{is_valid_hash, Bundle};
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct BundleInfo {
    pub hash: String,
    pub label: Option<String>,
    pub files: Vec<String>,
}

impl Store {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        for sub in ["bundles", "releases", "logs"] {
            fs::create_dir_all(root.join(sub))
                .with_context(|| format!("cannot create {}", root.join(sub).display()))?;
        }
        // Absolute path: symlinks and spawned commands must not depend on the cwd.
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    fn bundle_path(&self, hash: &str) -> PathBuf {
        self.root.join("bundles").join(format!("{hash}.json"))
    }

    pub fn has_bundle(&self, hash: &str) -> bool {
        is_valid_hash(hash) && self.bundle_path(hash).is_file()
    }

    /// Stores a bundle under its own hash and returns that hash.
    pub fn put_bundle(&self, bundle: &Bundle) -> Result<String> {
        bundle.validate()?;
        let hash = bundle.hash();
        let path = self.bundle_path(&hash);
        // Always (re)write: this also repairs a corrupt copy. Rename keeps it atomic.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(bundle)?)?;
        fs::rename(&tmp, &path)?;
        Ok(hash)
    }

    pub fn get_bundle(&self, hash: &str) -> Result<Option<Bundle>> {
        if !is_valid_hash(hash) {
            return Ok(None);
        }
        let data = match fs::read(self.bundle_path(hash)) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let bundle: Bundle = serde_json::from_slice(&data).context("corrupt bundle file")?;
        if bundle.hash() != hash {
            bail!("bundle file for {hash} does not match its hash (corrupt)");
        }
        Ok(Some(bundle))
    }

    pub fn list_bundles(&self) -> Result<Vec<BundleInfo>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.root.join("bundles"))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Ok(Some(b)) = self.get_bundle(stem) {
                out.push(BundleInfo {
                    hash: stem.to_string(),
                    label: b.label.clone(),
                    files: b.files.keys().cloned().collect(),
                });
            }
        }
        out.sort_by(|a, b| a.label.cmp(&b.label).then(a.hash.cmp(&b.hash)));
        Ok(out)
    }

    pub fn release_dir(&self, hash: &str) -> PathBuf {
        self.root.join("releases").join(hash)
    }

    /// Unpacks a bundle into `releases/<hash>/`, starting from a clean directory
    /// (a half-written directory from an earlier crash is never trusted).
    pub fn materialize(&self, bundle: &Bundle) -> Result<PathBuf> {
        bundle.validate()?;
        let hash = bundle.hash();
        let dir = self.release_dir(&hash);
        if self.current_hash().as_deref() == Some(hash.as_str()) {
            return Ok(dir); // already the active release; leave it alone
        }
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::create_dir_all(&dir)?;
        for (rel, content) in &bundle.files {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, content)?;
        }
        Ok(dir)
    }

    pub fn remove_release(&self, hash: &str) {
        if is_valid_hash(hash) && self.current_hash().as_deref() != Some(hash) {
            let _ = fs::remove_dir_all(self.release_dir(hash));
        }
    }

    fn link_target_hash(&self, name: &str) -> Option<String> {
        fs::read_link(self.root.join(name))
            .ok()
            .and_then(|t| t.file_name().map(|n| n.to_string_lossy().into_owned()))
    }

    pub fn current_hash(&self) -> Option<String> {
        self.link_target_hash("current")
    }

    pub fn previous_hash(&self) -> Option<String> {
        self.link_target_hash("previous")
    }

    /// Atomically makes `hash` the active release. The old one becomes `previous`.
    pub fn switch_to(&self, hash: &str) -> Result<()> {
        if !is_valid_hash(hash) || !self.release_dir(hash).is_dir() {
            bail!("release {hash} is not installed");
        }
        if let Some(old) = self.current_hash() {
            if old != hash {
                self.atomic_link("previous", &old)?;
            }
        }
        self.atomic_link("current", hash)
    }

    fn atomic_link(&self, name: &str, hash: &str) -> Result<()> {
        let tmp = self.root.join(format!(".{name}.tmp"));
        let _ = fs::remove_file(&tmp);
        // relative target: stays valid if the data directory is moved
        std::os::unix::fs::symlink(Path::new("releases").join(hash), &tmp)?;
        fs::rename(&tmp, self.root.join(name))?; // atomic replace
        Ok(())
    }

    /// Deletes installed releases other than `current` and `previous`.
    pub fn gc(&self) -> Result<Vec<String>> {
        let keep: HashSet<String> = [self.current_hash(), self.previous_hash()]
            .into_iter()
            .flatten()
            .collect();
        let mut removed = Vec::new();
        for entry in fs::read_dir(self.root.join("releases"))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !keep.contains(&name) {
                fs::remove_dir_all(entry.path())?;
                removed.push(name);
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn bundle(tag: &str) -> Bundle {
        let mut files = BTreeMap::new();
        files.insert("pixi.toml".to_string(), format!("# {tag}"));
        files.insert("pixi.lock".to_string(), "lock".to_string());
        files.insert("sub/run.sh".to_string(), "echo hi".to_string());
        Bundle {
            label: Some(tag.into()),
            files,
        }
    }

    #[test]
    fn bundle_roundtrip_and_corruption_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let b = bundle("one");
        let hash = store.put_bundle(&b).unwrap();
        assert_eq!(store.get_bundle(&hash).unwrap().unwrap(), b);
        assert!(store.get_bundle(&"0".repeat(64)).unwrap().is_none());
        assert!(store.get_bundle("../../etc/passwd").unwrap().is_none());
        // tamper with the file on disk
        let path = tmp.path().join("bundles").join(format!("{hash}.json"));
        let tampered = fs::read_to_string(&path)
            .unwrap()
            .replace("# one", "# evil");
        fs::write(&path, tampered).unwrap();
        assert!(store.get_bundle(&hash).is_err());
    }

    #[test]
    fn switch_is_atomic_keeps_previous_and_gc_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let (a, b, c) = (bundle("a"), bundle("b"), bundle("c"));
        for bun in [&a, &b, &c] {
            store.materialize(bun).unwrap();
        }
        store.switch_to(&a.hash()).unwrap();
        assert_eq!(store.current_hash(), Some(a.hash()));
        assert_eq!(store.previous_hash(), None);
        store.switch_to(&b.hash()).unwrap();
        assert_eq!(store.current_hash(), Some(b.hash()));
        assert_eq!(store.previous_hash(), Some(a.hash()));
        let removed = store.gc().unwrap();
        assert_eq!(removed, vec![c.hash()]);
        assert!(store.release_dir(&a.hash()).is_dir());
        assert!(store.release_dir(&b.hash()).is_dir());
        // switching to something that is not installed must fail and change nothing
        assert!(store.switch_to(&c.hash()).is_err());
        assert_eq!(store.current_hash(), Some(b.hash()));
    }

    #[test]
    fn materialize_writes_nested_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let dir = store.materialize(&bundle("x")).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("sub/run.sh")).unwrap(),
            "echo hi"
        );
    }
}
