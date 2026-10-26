//! Safety policy: protected locations, category-root validation, and risk
//! classification of cleanup candidates.
//!
//! All path comparisons are component-based (`Path::starts_with`), never
//! string-prefix based, so `/Users/ann2` is not considered inside `/Users/ann`.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Risk {
    /// Verified to belong to the category; eligible for deletion.
    Safe,
    /// Not recognized; shown to the user but never deleted.
    Review,
    /// Violates policy; never deleted.
    Forbidden,
}

/// Locations that a category root may never be, contain, or equal.
fn protected_locations(home: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = [
        "/",
        "/System",
        "/Library",
        "/Applications",
        "/Users",
        "/Volumes",
        "/private",
        "/usr",
        "/bin",
        "/sbin",
        "/etc",
        "/var",
        "/opt",
        "/cores",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    v.push(home.to_path_buf());
    for rel in [
        "Desktop",
        "Documents",
        "Downloads",
        "Movies",
        "Music",
        "Pictures",
        "Public",
        "Applications",
        "Library",
        "Library/Caches",
        "Library/Developer",
        "Library/Developer/Xcode",
    ] {
        v.push(home.join(rel));
    }
    v
}

/// Subtrees whose contents are never eligible for cleanup.
fn sensitive_subtrees(home: &Path) -> Vec<PathBuf> {
    [
        "Library/Keychains",
        "Library/Application Support",
        "Library/Containers",
        "Library/Group Containers",
        "Library/Mobile Documents",
        "Library/Mail",
        "Library/Messages",
        "Library/Safari",
        "Library/Cookies",
        "Library/Preferences",
        "Library/Accounts",
        "Library/CloudStorage",
        "Library/Developer/CoreSimulator",
        "Library/Developer/Xcode/Archives",
        ".ssh",
        ".gnupg",
        ".aws",
        ".kube",
        ".docker",
        ".config",
        ".git",
        "Documents",
        "Desktop",
        "Pictures",
    ]
    .iter()
    .map(|r| home.join(r))
    .collect()
}

/// True when `path` is `root` or lies beneath it, compared by components.
pub fn is_within(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

fn has_only_normal_components(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

/// Validates that `root` may serve as a cleanup category root for a user
/// whose (canonical) home directory is `home`.
pub fn validate_category_root(root: &Path, home: &Path) -> Result<(), String> {
    if !has_only_normal_components(root) {
        return Err(format!(
            "{} is not an absolute path free of '.' and '..' components",
            root.display()
        ));
    }
    if !is_within(root, home) || root == home {
        return Err(format!(
            "{} is outside the home directory {}",
            root.display(),
            home.display()
        ));
    }
    for p in protected_locations(home) {
        if root == p || is_within(&p, root) {
            return Err(format!(
                "{} is, or contains, the protected location {}",
                root.display(),
                p.display()
            ));
        }
    }
    for s in sensitive_subtrees(home) {
        if is_within(root, &s) {
            return Err(format!(
                "{} lies inside the protected area {}",
                root.display(),
                s.display()
            ));
        }
    }
    // A cache root that is (or is inside) a Git work tree is not a cache.
    if root
        .components()
        .any(|c| matches!(c, Component::Normal(n) if n == ".git"))
    {
        return Err(format!("{} is inside a Git directory", root.display()));
    }
    Ok(())
}

/// Classification of one entry directly under a category root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub risk: Risk,
    pub reason: String,
}

impl Verdict {
    pub fn safe(reason: impl Into<String>) -> Self {
        Verdict {
            risk: Risk::Safe,
            reason: reason.into(),
        }
    }
    pub fn review(reason: impl Into<String>) -> Self {
        Verdict {
            risk: Risk::Review,
            reason: reason.into(),
        }
    }
    pub fn forbidden(reason: impl Into<String>) -> Self {
        Verdict {
            risk: Risk::Forbidden,
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/Users/ann";

    fn check(p: &str) -> Result<(), String> {
        validate_category_root(Path::new(p), Path::new(HOME))
    }

    #[test]
    fn allows_narrow_cache_roots() {
        assert!(check("/Users/ann/.npm/_cacache").is_ok());
        assert!(check("/Users/ann/Library/Developer/Xcode/DerivedData").is_ok());
    }

    #[test]
    fn rejects_protected_and_broad_roots() {
        for p in [
            "/",
            "/Users",
            "/Users/ann",
            "/Users/ann/Library",
            "/Users/ann/Library/Caches",
            "/Users/ann/Documents",
            "/Users/ann/Documents/npm-cache",
            "/Users/ann/Library/Keychains/x",
            "/Users/ann/Library/Application Support/npm",
            "/Users/ann/.ssh/cache",
            "/Users/ann/project/.git/cache",
            "/Users/ann/Library/Developer",
        ] {
            assert!(check(p).is_err(), "{p} should be rejected");
        }
    }

    #[test]
    fn rejects_outside_home_without_string_prefix_confusion() {
        assert!(check("/Users/ann2/.npm/_cacache").is_err());
        assert!(check("/Users/annex/.npm").is_err());
        assert!(check("/tmp/.npm/_cacache").is_err());
        assert!(check("/Library/Caches/x").is_err());
    }

    #[test]
    fn rejects_traversal_and_relative_paths() {
        assert!(check("/Users/ann/.npm/../Documents").is_err());
        assert!(check("Users/ann/.npm/_cacache").is_err());
        assert!(check("/Users/ann/.npm/../../ann/.npm/_cacache").is_err());
    }

    #[test]
    fn is_within_is_component_based() {
        assert!(is_within(Path::new("/a/b/c"), Path::new("/a/b")));
        assert!(!is_within(Path::new("/a/bc"), Path::new("/a/b")));
    }
}
