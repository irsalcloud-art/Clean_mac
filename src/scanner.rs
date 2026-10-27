//! Read-only storage analysis.
//!
//! The scanner never modifies the filesystem. It uses `lstat` semantics
//! (symbolic links are counted as links, never followed), keeps only the top
//! N results in bounded heaps, and records every path it could not read.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::platform::fsops::FileId;

/// How many skipped paths are listed individually; the total is always counted.
pub const MAX_RECORDED_SKIPS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum SizeMetric {
    /// Storage allocated on disk (st_blocks × 512).
    Allocated,
    /// Logical file length (st_size).
    Logical,
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub roots: Vec<PathBuf>,
    pub top: usize,
    pub sort_by: SizeMetric,
    /// Descend into directories that live on a different filesystem.
    pub cross_mounts: bool,
    /// Rank only directories at most this many levels below a root (the
    /// root itself is level 0). Sizes still include everything below.
    pub max_depth: Option<usize>,
    /// Entries to leave out entirely: not entered, not counted.
    pub exclude: Vec<ExcludePattern>,
    /// Directories not descended into because a cloud app syncs them (see
    /// [`cloud_folders`]). A root that is one of them is still scanned.
    pub skip_cloud: Vec<FileId>,
}

/// Folders under `home` whose contents a sync app provides on demand:
/// `Library/CloudStorage` (Google Drive, Dropbox, OneDrive and other File
/// Provider apps) and `Library/Mobile Documents` (iCloud Drive). Listing them
/// can wait on the app and make it fetch data, so `analyze` skips them
/// unless asked. Identified by device and inode, so any spelling of the
/// path matches. Folders that do not exist are left out.
pub fn cloud_folders(home: &Path) -> Vec<FileId> {
    ["Library/CloudStorage", "Library/Mobile Documents"]
        .iter()
        .filter_map(|rel| fs::symlink_metadata(home.join(rel)).ok())
        .filter(|m| m.is_dir())
        .map(|m| FileId {
            dev: m.dev(),
            ino: m.ino(),
        })
        .collect()
}

/// A shell glob (`*`, `?`, `[...]`, via fnmatch(3)). Without a `/` it is
/// matched against an entry's name at any depth; with a `/` against the
/// entry's path relative to the scanned root, where `*` does not cross `/`.
#[derive(Debug, Clone)]
pub struct ExcludePattern {
    text: String,
    glob: CString,
    whole_path: bool,
}

impl ExcludePattern {
    pub fn new(text: &str) -> Result<ExcludePattern, String> {
        let trimmed = text.trim_matches('/');
        if trimmed.is_empty() {
            return Err("an --exclude pattern must not be empty".into());
        }
        let glob = CString::new(trimmed)
            .map_err(|_| "an --exclude pattern must not contain a NUL byte".to_string())?;
        Ok(ExcludePattern {
            text: text.to_string(),
            whole_path: trimmed.contains('/'),
            glob,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// `name` is the entry's own name, `relative` its path below the root.
    pub fn matches(&self, name: &OsStr, relative: &Path) -> bool {
        let (subject, flags) = if self.whole_path {
            (relative.as_os_str(), libc::FNM_PATHNAME)
        } else {
            (name, 0)
        };
        let Ok(subject) = CString::new(subject.as_bytes()) else {
            return false;
        };
        // SAFETY: both arguments are valid NUL-terminated strings.
        unsafe { libc::fnmatch(self.glob.as_ptr(), subject.as_ptr(), flags) == 0 }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SizedPath {
    pub path: PathBuf,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    /// Files contained (directories only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    PermissionDenied,
    /// The entry was listed but disappeared before it could be examined.
    Vanished,
    /// The root is a symbolic link; links are not followed.
    SymlinkRootNotFollowed,
    /// The directory is already being scanned higher up (filesystem cycle).
    Cycle,
    Error(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedPath {
    pub path: PathBuf,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanTotals {
    pub files: u64,
    pub directories: u64,
    pub symlinks_not_followed: u64,
    pub other_entries: u64,
    /// Entries left out by `--exclude` (subtrees count once).
    pub excluded_entries: u64,
    /// Files with several hard links are counted once.
    pub hardlinks_deduplicated: u64,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    pub roots: Vec<PathBuf>,
    pub sort_by: SizeMetric,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    pub totals: ScanTotals,
    pub top_files: Vec<SizedPath>,
    pub top_directories: Vec<SizedPath>,
    /// True when any part of the requested tree could not be read.
    pub incomplete: bool,
    pub skipped_total: u64,
    pub skipped: Vec<SkippedPath>,
    /// Mount points intentionally not crossed (not counted as incomplete).
    pub mounts_not_crossed: Vec<PathBuf>,
    /// Cloud-synced folders intentionally not entered (not counted as
    /// incomplete); `--include-cloud` scans them.
    pub cloud_folders_skipped: Vec<PathBuf>,
    pub elapsed_ms: u128,
}

/// Heap entry ordered by size, ties broken by discovery order.
struct Ranked {
    key: u64,
    tie: u64,
    item: SizedPath,
}

impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        (self.key, self.tie) == (other.key, other.tie)
    }
}
impl Eq for Ranked {}
impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.key, self.tie).cmp(&(other.key, other.tie))
    }
}

/// Keeps the N largest items seen so far in O(N) memory.
struct TopN {
    limit: usize,
    heap: BinaryHeap<Reverse<Ranked>>,
    seq: u64,
}

impl TopN {
    fn new(limit: usize) -> Self {
        TopN {
            limit,
            heap: BinaryHeap::with_capacity(limit + 1),
            seq: 0,
        }
    }

    /// `make` is only called when the item will be kept, avoiding allocations.
    fn offer(&mut self, key: u64, make: impl FnOnce() -> SizedPath) {
        let accept = self.limit > 0
            && (self.heap.len() < self.limit
                || self.heap.peek().is_some_and(|Reverse(min)| key > min.key));
        if !accept {
            return;
        }
        // Earlier-seen items win ties, keeping output deterministic.
        self.seq += 1;
        self.heap.push(Reverse(Ranked {
            key,
            tie: u64::MAX - self.seq,
            item: make(),
        }));
        if self.heap.len() > self.limit {
            self.heap.pop();
        }
    }

    fn into_sorted(self) -> Vec<SizedPath> {
        let mut v: Vec<Ranked> = self.heap.into_iter().map(|Reverse(r)| r).collect();
        v.sort_by(|a, b| b.cmp(a));
        v.into_iter().map(|r| r.item).collect()
    }
}

#[derive(Default, Clone, Copy)]
struct Agg {
    logical: u64,
    allocated: u64,
    files: u64,
}

struct Frame {
    path: PathBuf,
    id: FileId,
    pending: std::vec::IntoIter<OsString>,
    agg: Agg,
}

/// Test hook invoked after a directory has been listed and before its
/// entries are examined. Used to simulate concurrent filesystem changes.
pub type ListingHook<'a> = &'a mut dyn FnMut(&Path);

struct Scanner<'h> {
    opts: ScanOptions,
    totals: ScanTotals,
    files: TopN,
    dirs: TopN,
    seen_hardlinks: HashSet<FileId>,
    skipped: Vec<SkippedPath>,
    skipped_total: u64,
    mounts_not_crossed: Vec<PathBuf>,
    cloud_folders_skipped: Vec<PathBuf>,
    hook: Option<ListingHook<'h>>,
}

fn classify(err: &io::Error) -> SkipReason {
    match err.kind() {
        io::ErrorKind::PermissionDenied => SkipReason::PermissionDenied,
        io::ErrorKind::NotFound => SkipReason::Vanished,
        _ => SkipReason::Error(err.to_string()),
    }
}

impl<'h> Scanner<'h> {
    fn key(&self, logical: u64, allocated: u64) -> u64 {
        match self.opts.sort_by {
            SizeMetric::Allocated => allocated,
            SizeMetric::Logical => logical,
        }
    }

    fn skip(&mut self, path: PathBuf, reason: SkipReason) {
        self.skipped_total += 1;
        if self.skipped.len() < MAX_RECORDED_SKIPS {
            self.skipped.push(SkippedPath { path, reason });
        }
    }

    fn list(&mut self, path: &Path) -> io::Result<Vec<OsString>> {
        // Collect names and close the directory immediately so open
        // descriptors do not grow with tree depth.
        let mut names = Vec::new();
        for entry in fs::read_dir(path)? {
            names.push(entry?.file_name());
        }
        if let Some(hook) = self.hook.as_mut() {
            hook(path);
        }
        Ok(names)
    }

    fn scan_root(&mut self, root: &Path) {
        let meta = match fs::symlink_metadata(root) {
            Ok(m) => m,
            Err(e) => {
                let reason = classify(&e);
                self.skip(root.to_path_buf(), reason);
                return;
            }
        };
        let ft = meta.file_type();
        if ft.is_symlink() {
            self.totals.symlinks_not_followed += 1;
            self.skip(root.to_path_buf(), SkipReason::SymlinkRootNotFollowed);
            return;
        }
        if ft.is_file() {
            self.account_file(root, &meta);
            return;
        }
        if !ft.is_dir() {
            self.totals.other_entries += 1;
            return;
        }
        let root_dev = meta.dev();
        self.totals.directories += 1;
        let names = match self.list(root) {
            Ok(n) => n,
            Err(e) => {
                let reason = classify(&e);
                self.skip(root.to_path_buf(), reason);
                return;
            }
        };
        let mut stack = vec![Frame {
            path: root.to_path_buf(),
            id: FileId {
                dev: meta.dev(),
                ino: meta.ino(),
            },
            pending: names.into_iter(),
            agg: Agg::default(),
        }];

        while let Some(top) = stack.last_mut() {
            let Some(name) = top.pending.next() else {
                let depth = stack.len() - 1;
                let done = stack.pop().expect("stack is non-empty");
                self.finish_dir(&done, depth);
                if let Some(parent) = stack.last_mut() {
                    parent.agg.logical += done.agg.logical;
                    parent.agg.allocated += done.agg.allocated;
                    parent.agg.files += done.agg.files;
                }
                continue;
            };
            let path = top.path.join(&name);
            if self.excluded(&name, &path, root) {
                self.totals.excluded_entries += 1;
                continue;
            }
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    let reason = classify(&e);
                    self.skip(path, reason);
                    continue;
                }
            };
            let ft = meta.file_type();
            if ft.is_symlink() {
                self.totals.symlinks_not_followed += 1;
            } else if ft.is_file() {
                if let Some((l, a)) = self.account_file(&path, &meta) {
                    let top = stack.last_mut().expect("stack is non-empty");
                    top.agg.logical += l;
                    top.agg.allocated += a;
                    top.agg.files += 1;
                }
            } else if ft.is_dir() {
                let id = FileId {
                    dev: meta.dev(),
                    ino: meta.ino(),
                };
                if meta.dev() != root_dev && !self.opts.cross_mounts {
                    self.mounts_not_crossed.push(path);
                    continue;
                }
                if self.opts.skip_cloud.contains(&id) {
                    self.cloud_folders_skipped.push(path);
                    continue;
                }
                if stack.iter().any(|f| f.id == id) {
                    self.skip(path, SkipReason::Cycle);
                    continue;
                }
                self.totals.directories += 1;
                match self.list(&path) {
                    Ok(names) => stack.push(Frame {
                        path,
                        id,
                        pending: names.into_iter(),
                        agg: Agg::default(),
                    }),
                    Err(e) => {
                        let reason = classify(&e);
                        self.skip(path, reason);
                    }
                }
            } else {
                self.totals.other_entries += 1;
            }
        }
    }

    /// Returns the bytes attributed to this file, or None when it is an
    /// additional hard link to a file that was already counted.
    fn account_file(&mut self, path: &Path, meta: &fs::Metadata) -> Option<(u64, u64)> {
        if meta.nlink() > 1
            && !self.seen_hardlinks.insert(FileId {
                dev: meta.dev(),
                ino: meta.ino(),
            })
        {
            self.totals.hardlinks_deduplicated += 1;
            return None;
        }
        let logical = meta.len();
        let allocated = meta.blocks().saturating_mul(512);
        self.totals.files += 1;
        self.totals.logical_bytes += logical;
        self.totals.allocated_bytes += allocated;
        let key = self.key(logical, allocated);
        self.files.offer(key, || SizedPath {
            path: path.to_path_buf(),
            logical_bytes: logical,
            allocated_bytes: allocated,
            file_count: None,
        });
        Some((logical, allocated))
    }

    fn excluded(&self, name: &OsStr, path: &Path, root: &Path) -> bool {
        if self.opts.exclude.is_empty() {
            return false;
        }
        let relative = path.strip_prefix(root).unwrap_or(path);
        self.opts.exclude.iter().any(|p| p.matches(name, relative))
    }

    /// `depth` is 0 for a root, 1 for its subdirectories, and so on.
    fn finish_dir(&mut self, frame: &Frame, depth: usize) {
        if self.opts.max_depth.is_some_and(|max| depth > max) {
            return;
        }
        let key = self.key(frame.agg.logical, frame.agg.allocated);
        self.dirs.offer(key, || SizedPath {
            path: frame.path.clone(),
            logical_bytes: frame.agg.logical,
            allocated_bytes: frame.agg.allocated,
            file_count: Some(frame.agg.files),
        });
    }
}

pub fn scan(opts: ScanOptions) -> ScanReport {
    scan_with_hook(opts, None)
}

pub fn scan_with_hook(opts: ScanOptions, hook: Option<ListingHook<'_>>) -> ScanReport {
    let started = Instant::now();
    let top = opts.top;
    let mut s = Scanner {
        totals: ScanTotals::default(),
        files: TopN::new(top),
        dirs: TopN::new(top),
        seen_hardlinks: HashSet::new(),
        skipped: Vec::new(),
        skipped_total: 0,
        mounts_not_crossed: Vec::new(),
        cloud_folders_skipped: Vec::new(),
        hook,
        opts,
    };
    for root in s.opts.roots.clone() {
        s.scan_root(&root);
    }

    let Scanner {
        opts,
        totals,
        files,
        dirs,
        skipped,
        skipped_total,
        mounts_not_crossed,
        cloud_folders_skipped,
        ..
    } = s;

    ScanReport {
        roots: opts.roots,
        sort_by: opts.sort_by,
        max_depth: opts.max_depth,
        exclude: opts
            .exclude
            .iter()
            .map(|p| p.as_str().to_string())
            .collect(),
        totals,
        top_files: files.into_sorted(),
        top_directories: dirs.into_sorted(),
        incomplete: skipped_total > 0,
        skipped_total,
        skipped,
        mounts_not_crossed,
        cloud_folders_skipped,
        elapsed_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn opts(root: &Path) -> ScanOptions {
        ScanOptions {
            roots: vec![root.to_path_buf()],
            top: 10,
            sort_by: SizeMetric::Logical,
            max_depth: None,
            exclude: Vec::new(),
            skip_cloud: Vec::new(),
            cross_mounts: false,
        }
    }

    fn write(path: &Path, len: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![7u8; len]).unwrap();
    }

    #[test]
    fn file_sizes_are_exact_and_allocation_is_reported() {
        let t = tempfile::tempdir().unwrap();
        write(&t.path().join("a.bin"), 10_000);
        write(&t.path().join("b.bin"), 1);
        let r = scan(opts(t.path()));
        assert_eq!(r.totals.files, 2);
        assert_eq!(r.totals.logical_bytes, 10_001);
        assert!(r.totals.allocated_bytes >= 10_001);
        assert_eq!(r.top_files[0].path, t.path().join("a.bin"));
        assert_eq!(r.top_files[0].logical_bytes, 10_000);
        assert!(!r.incomplete);
    }

    #[test]
    fn directories_aggregate_recursively() {
        let t = tempfile::tempdir().unwrap();
        write(&t.path().join("x/1"), 100);
        write(&t.path().join("x/y/2"), 200);
        write(&t.path().join("x/y/z/3"), 300);
        write(&t.path().join("w/4"), 50);
        let r = scan(opts(t.path()));
        let get = |rel: &str| {
            r.top_directories
                .iter()
                .find(|d| d.path == t.path().join(rel))
                .unwrap()
                .clone()
        };
        assert_eq!(get("x").logical_bytes, 600);
        assert_eq!(get("x").file_count, Some(3));
        assert_eq!(get("x/y").logical_bytes, 500);
        assert_eq!(get("x/y/z").logical_bytes, 300);
        assert_eq!(get("w").logical_bytes, 50);
        assert_eq!(r.top_directories[0].path, t.path());
        assert_eq!(r.top_directories[0].logical_bytes, 650);
        assert_eq!(r.totals.directories, 5);
    }

    #[test]
    fn top_limit_is_respected_and_sorted() {
        let t = tempfile::tempdir().unwrap();
        for i in 1..=20 {
            write(&t.path().join(format!("f{i}")), i * 10);
        }
        let mut o = opts(t.path());
        o.top = 3;
        let r = scan(o);
        let sizes: Vec<u64> = r.top_files.iter().map(|f| f.logical_bytes).collect();
        assert_eq!(sizes, vec![200, 190, 180]);
        assert_eq!(r.totals.files, 20);
    }

    #[test]
    fn hard_links_are_counted_once() {
        let t = tempfile::tempdir().unwrap();
        write(&t.path().join("orig"), 4096);
        fs::hard_link(t.path().join("orig"), t.path().join("link")).unwrap();
        let r = scan(opts(t.path()));
        assert_eq!(r.totals.files, 1);
        assert_eq!(r.totals.hardlinks_deduplicated, 1);
        assert_eq!(r.totals.logical_bytes, 4096);
    }

    #[test]
    fn symlinks_are_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("big"), 50_000);
        write(&t.path().join("small"), 10);
        std::os::unix::fs::symlink(outside.path(), t.path().join("dirlink")).unwrap();
        std::os::unix::fs::symlink(t.path(), t.path().join("loop")).unwrap();
        let r = scan(opts(t.path()));
        assert_eq!(r.totals.logical_bytes, 10);
        assert_eq!(r.totals.symlinks_not_followed, 2);
        assert!(!r.incomplete);
    }

    #[test]
    fn symlink_root_is_reported_not_followed() {
        let t = tempfile::tempdir().unwrap();
        write(&t.path().join("real/f"), 10);
        std::os::unix::fs::symlink(t.path().join("real"), t.path().join("link")).unwrap();
        let r = scan(opts(&t.path().join("link")));
        assert_eq!(r.totals.files, 0);
        assert!(r.incomplete);
        assert!(matches!(
            r.skipped[0].reason,
            SkipReason::SymlinkRootNotFollowed
        ));
    }

    #[test]
    fn permission_denied_is_reported_and_scan_continues() {
        let t = tempfile::tempdir().unwrap();
        write(&t.path().join("locked/secret"), 999);
        write(&t.path().join("open/visible"), 5);
        let locked = t.path().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let r = scan(opts(t.path()));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(r.incomplete);
        assert_eq!(r.skipped_total, 1);
        assert_eq!(r.skipped[0].path, locked);
        assert!(matches!(r.skipped[0].reason, SkipReason::PermissionDenied));
        assert_eq!(r.totals.logical_bytes, 5);
    }

    #[test]
    fn missing_root_is_reported() {
        let t = tempfile::tempdir().unwrap();
        let r = scan(opts(&t.path().join("nope")));
        assert!(r.incomplete);
        assert!(matches!(r.skipped[0].reason, SkipReason::Vanished));
    }

    #[test]
    fn files_disappearing_during_scan_are_reported() {
        let t = tempfile::tempdir().unwrap();
        write(&t.path().join("d/keep"), 10);
        write(&t.path().join("d/gone"), 20);
        write(&t.path().join("d/sub/inner"), 30);
        let d = t.path().join("d");
        let mut hook = |listed: &Path| {
            if listed == d {
                fs::remove_file(d.join("gone")).unwrap();
                fs::remove_dir_all(d.join("sub")).unwrap();
            }
        };
        let r = scan_with_hook(opts(t.path()), Some(&mut hook));
        assert!(r.incomplete);
        assert_eq!(r.skipped_total, 2);
        assert!(
            r.skipped
                .iter()
                .all(|s| matches!(s.reason, SkipReason::Vanished))
        );
        assert_eq!(r.totals.logical_bytes, 10);
    }

    #[test]
    fn scanner_never_modifies_files() {
        let t = tempfile::tempdir().unwrap();
        write(&t.path().join("a/b/c"), 123);
        let before = fs::metadata(t.path().join("a/b/c"))
            .unwrap()
            .modified()
            .unwrap();
        let _ = scan(opts(t.path()));
        let after = fs::metadata(t.path().join("a/b/c"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(fs::read(t.path().join("a/b/c")).unwrap().len(), 123);
    }

    fn dir_paths(r: &ScanReport) -> Vec<PathBuf> {
        r.top_directories.iter().map(|d| d.path.clone()).collect()
    }

    #[test]
    fn max_depth_limits_ranked_directories_but_not_sizes() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        write(&root.join("a/b/c/deep"), 1000);
        write(&root.join("a/top"), 10);
        write(&root.join("x/y"), 5);

        let mut o = opts(&root);
        o.max_depth = Some(1);
        let r = scan(o);
        let mut dirs = dir_paths(&r);
        dirs.sort();
        assert_eq!(dirs, vec![root.clone(), root.join("a"), root.join("x")]);
        let a = r
            .top_directories
            .iter()
            .find(|d| d.path == root.join("a"))
            .unwrap();
        // The deep file still counts towards its ancestors.
        assert_eq!(a.logical_bytes, 1010);
        assert_eq!(a.file_count, Some(2));
        assert_eq!(r.totals.files, 3);
        assert_eq!(r.top_files[0].path, root.join("a/b/c/deep"));
        assert_eq!(r.max_depth, Some(1));

        let mut o = opts(&root);
        o.max_depth = Some(0);
        assert_eq!(dir_paths(&scan(o)), vec![root.clone()]);

        // Without a limit, the nested chain is ranked too.
        assert!(dir_paths(&scan(opts(&root))).contains(&root.join("a/b/c")));
    }

    #[test]
    fn excluded_names_are_neither_entered_nor_counted() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        write(&root.join("app/node_modules/pkg/index.js"), 5000);
        write(&root.join("app/src/main.rs"), 100);
        write(&root.join("app/debug.log"), 70);
        write(&root.join("node_modules/x"), 9);

        let mut o = opts(&root);
        o.exclude = vec![
            ExcludePattern::new("node_modules").unwrap(),
            ExcludePattern::new("*.log").unwrap(),
        ];
        let r = scan(o);
        assert_eq!(r.totals.files, 1);
        assert_eq!(r.totals.logical_bytes, 100);
        assert_eq!(r.totals.excluded_entries, 3);
        assert!(!r.incomplete);
        assert!(
            dir_paths(&r)
                .iter()
                .all(|p| !p.components().any(|c| c.as_os_str() == "node_modules"))
        );
        assert_eq!(r.exclude, vec!["node_modules", "*.log"]);
    }

    #[test]
    fn exclude_patterns_with_a_slash_match_the_path_below_the_root() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        write(&root.join("a/b/f"), 10);
        write(&root.join("x/a/b/f"), 20);
        write(&root.join("a/c/d/f"), 40);

        let mut o = opts(&root);
        o.exclude = vec![ExcludePattern::new("a/b").unwrap()];
        let r = scan(o);
        // Only root/a/b goes, not root/x/a/b.
        assert_eq!(r.totals.logical_bytes, 60);

        // `*` does not cross `/`: `a/*` matches a/c but a/c/d is never reached.
        let mut o = opts(&root);
        o.exclude = vec![ExcludePattern::new("a/*").unwrap()];
        let r = scan(o);
        assert_eq!(r.totals.logical_bytes, 20);
        assert_eq!(r.totals.excluded_entries, 2);
    }

    #[test]
    fn excluded_symlinks_are_not_followed_and_roots_are_never_excluded() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap().join("data");
        write(&root.join("keep"), 10);
        std::os::unix::fs::symlink(t.path(), root.join("link")).unwrap();
        let mut o = opts(&root);
        o.exclude = vec![
            ExcludePattern::new("li*").unwrap(),
            ExcludePattern::new("data").unwrap(),
        ];
        let r = scan(o);
        assert_eq!(r.totals.excluded_entries, 1);
        assert_eq!(r.totals.symlinks_not_followed, 0);
        // The root itself matches `data` but was asked for explicitly.
        assert_eq!(r.totals.logical_bytes, 10);
    }

    #[test]
    fn invalid_exclude_patterns_are_rejected() {
        assert!(ExcludePattern::new("").is_err());
        assert!(ExcludePattern::new("/").is_err());
        assert!(ExcludePattern::new("a\0b").is_err());
        assert!(ExcludePattern::new("/build/").is_ok());
    }

    #[test]
    fn cloud_folders_are_skipped_unless_scanned_directly() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().canonicalize().unwrap();
        write(&home.join("Library/CloudStorage/GoogleDrive-a/doc"), 4000);
        write(
            &home.join("Library/Mobile Documents/com~apple~CloudDocs/note"),
            2000,
        );
        write(&home.join("Library/Caches/c"), 30);
        write(&home.join("file"), 5);

        let cloud = cloud_folders(&home);
        assert_eq!(cloud.len(), 2);
        let mut o = opts(&home);
        o.skip_cloud = cloud.clone();
        let r = scan(o);
        assert_eq!(r.totals.logical_bytes, 35);
        assert!(!r.incomplete);
        let mut skipped = r.cloud_folders_skipped.clone();
        skipped.sort();
        assert_eq!(
            skipped,
            vec![
                home.join("Library/CloudStorage"),
                home.join("Library/Mobile Documents")
            ]
        );

        // Passing a cloud folder itself scans it: it was asked for.
        let mut o = opts(&home.join("Library/CloudStorage"));
        o.skip_cloud = cloud.clone();
        let r = scan(o);
        assert_eq!(r.totals.logical_bytes, 4000);
        assert!(r.cloud_folders_skipped.is_empty());

        // An empty skip list (--include-cloud) enters everything.
        assert_eq!(scan(opts(&home)).totals.logical_bytes, 6035);

        // Missing folders are simply absent from the list.
        let empty = tempfile::tempdir().unwrap();
        assert!(cloud_folders(empty.path()).is_empty());
    }
}
