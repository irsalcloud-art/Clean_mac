//! Cleanup planning (read-only) and guarded execution.
//!
//! Planning resolves each category root, validates it against the policy,
//! classifies the root's entries, and measures eligible candidates. It never
//! deletes anything. Execution re-resolves and re-opens the root, checks
//! that every candidate is still the same filesystem object, and then
//! deletes descriptor-relatively without following symbolic links.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::errors::{MacCleanError, Result};
use crate::platform::fsops::{self, FileId, FileKind, Meta, borrow};
use crate::platform::{self, VolumeStats};
use crate::policy::{self, Risk, Verdict};

/// How many skipped items and errors are listed individually per category.
pub const MAX_RECORDED_ITEMS: usize = 100;
/// Directory nesting below a candidate beyond which MacClean refuses to go.
pub const MAX_DEPTH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum CategoryId {
    /// npm's content-addressable cache (`<npm cache>/_cacache`).
    NpmCache,
    /// Xcode build products and indexes in `~/Library/Developer/Xcode/DerivedData`.
    XcodeDerivedData,
}

impl CategoryId {
    pub const ALL: [CategoryId; 2] = [CategoryId::NpmCache, CategoryId::XcodeDerivedData];

    pub fn name(self) -> &'static str {
        match self {
            CategoryId::NpmCache => "npm-cache",
            CategoryId::XcodeDerivedData => "xcode-derived-data",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            CategoryId::NpmCache => {
                "npm's content-addressable package cache (_cacache). npm re-downloads \
                 packages on demand; equivalent to `npm cache clean --force`. Logs and \
                 npx installs are not touched. Refused while npm, npx, yarn or pnpm is \
                 running."
            }
            CategoryId::XcodeDerivedData => {
                "Xcode build products, indexes and module caches. Xcode rebuilds them on \
                 the next build, which can take a while. Archives and simulators are not \
                 touched. Refused while Xcode or xcodebuild is running."
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum NpmCacheSource {
    /// Read npm's configuration files and environment (see [`detect_npm_cache_dir`]).
    Detect,
    /// Use this npm cache directory (the parent of `_cacache`).
    Explicit(PathBuf),
}

#[derive(Debug, Clone)]
pub struct CleanContext {
    /// Canonical home directory of the current user.
    pub home: PathBuf,
    pub uid: u32,
    pub npm_cache: NpmCacheSource,
    /// `None` means detect running Xcode processes.
    pub xcode_running: Option<bool>,
    /// `None` means detect running npm, npx, yarn or pnpm processes.
    pub package_manager_running: Option<bool>,
}

impl CleanContext {
    pub fn detect() -> Result<CleanContext> {
        let home = platform::home_dir()
            .ok_or_else(|| MacCleanError::Policy("cannot determine home directory".into()))?;
        let home = home
            .canonicalize()
            .map_err(|e| MacCleanError::io(&home, e))?;
        Ok(CleanContext {
            home,
            uid: platform::current_uid(),
            npm_cache: NpmCacheSource::Detect,
            xcode_running: None,
            package_manager_running: test_package_manager_override(),
        })
    }
}

/// Lets the end-to-end tests run the binary on a machine where a package
/// manager is busy (for example a long-running `npm exec` server). Only
/// debug builds read the variable; a release build always checks.
#[cfg(debug_assertions)]
fn test_package_manager_override() -> Option<bool> {
    std::env::var_os("MACCLEAN_TEST_ASSUME_NO_PACKAGE_MANAGER").map(|_| false)
}

#[cfg(not(debug_assertions))]
fn test_package_manager_override() -> Option<bool> {
    None
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemRecord {
    pub path: PathBuf,
    pub reason: String,
}

/// Counts of what was (or would be) removed, plus what was skipped.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Tally {
    pub files: u64,
    pub symlinks: u64,
    pub directories: u64,
    pub logical_bytes: u64,
    /// Sum of allocated blocks of removed files. Space actually returned to
    /// the volume can differ (APFS clones, snapshots, open files).
    pub allocated_bytes: u64,
    pub skipped_total: u64,
    pub skipped: Vec<ItemRecord>,
    pub errors_total: u64,
    pub errors: Vec<ItemRecord>,
}

impl Tally {
    fn skip(&mut self, path: &Path, reason: impl Into<String>) {
        self.skipped_total += 1;
        if self.skipped.len() < MAX_RECORDED_ITEMS {
            self.skipped.push(ItemRecord {
                path: path.to_path_buf(),
                reason: reason.into(),
            });
        }
    }

    fn error(&mut self, path: &Path, err: &io::Error) {
        self.errors_total += 1;
        if self.errors.len() < MAX_RECORDED_ITEMS {
            self.errors.push(ItemRecord {
                path: path.to_path_buf(),
                reason: err.to_string(),
            });
        }
    }

    pub fn absorb(&mut self, other: &Tally) {
        self.files += other.files;
        self.symlinks += other.symlinks;
        self.directories += other.directories;
        self.logical_bytes += other.logical_bytes;
        self.allocated_bytes += other.allocated_bytes;
        self.skipped_total += other.skipped_total;
        self.errors_total += other.errors_total;
        for s in &other.skipped {
            if self.skipped.len() < MAX_RECORDED_ITEMS {
                self.skipped.push(s.clone());
            }
        }
        for e in &other.errors {
            if self.errors.len() < MAX_RECORDED_ITEMS {
                self.errors.push(e.clone());
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    #[serde(skip)]
    pub name: OsString,
    pub path: PathBuf,
    pub kind: FileKind,
    #[serde(skip)]
    pub id: FileId,
    pub reason: String,
    /// What deletion would remove under the current policy.
    pub estimate: Tally,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewItem {
    pub path: PathBuf,
    pub risk: Risk,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CategoryState {
    /// Candidates were found and verified.
    Ready,
    /// The category root exists but holds nothing eligible.
    Empty,
    /// The category root does not exist on this Mac.
    NotPresent,
    /// The policy refused this category; nothing will be deleted.
    Refused,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryPlan {
    pub category: CategoryId,
    pub description: &'static str,
    pub state: CategoryState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_reason: Option<String>,
    pub root: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_source: Option<String>,
    #[serde(skip)]
    pub root_id: Option<FileId>,
    pub candidates: Vec<Candidate>,
    pub not_eligible: Vec<ReviewItem>,
    pub estimate: Tally,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanPlan {
    pub dry_run: bool,
    pub created_unix_secs: u64,
    /// Files changed after this instant are not deleted.
    #[serde(skip)]
    pub created_ns: i128,
    pub categories: Vec<CategoryPlan>,
}

impl CleanPlan {
    pub fn has_candidates(&self) -> bool {
        self.categories.iter().any(|c| !c.candidates.is_empty())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryOutcome {
    pub category: CategoryId,
    /// Set when the category was refused at execution time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// Candidates that failed revalidation and were not touched.
    pub rejected_candidates: Vec<ItemRecord>,
    pub removed: Tally,
    /// Change in the volume's available space observed around this
    /// category. Other activity on the Mac also affects it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_available_delta_bytes: Option<i128>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanOutcome {
    pub categories: Vec<CategoryOutcome>,
}

impl CleanOutcome {
    /// True when anything planned was not removed.
    pub fn is_partial(&self) -> bool {
        self.categories.iter().any(|c| {
            c.refused.is_some()
                || !c.rejected_candidates.is_empty()
                || c.removed.errors_total > 0
                || c.removed.skipped_total > 0
        })
    }
}

fn now_ns() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Root resolution
// ---------------------------------------------------------------------------

enum Resolution {
    Root { path: PathBuf, source: String },
    NotPresent(String),
    Refused(String),
}

/// Parses the value of a `cache=` line from npmrc-style text. The last
/// assignment wins, as in npm's ini parser.
fn npmrc_cache_value(text: &str) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("cache")
        {
            let v = value.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(v);
            found = Some(v.to_string());
        }
    }
    found
}

/// Turns a configured cache value into an absolute path, or explains why
/// it is too ambiguous to act on.
fn npm_cache_value_to_path(value: &str, home: &Path) -> std::result::Result<PathBuf, String> {
    if value.contains("${") {
        return Err(format!(
            "npm cache setting `{value}` uses variable expansion, which MacClean does not evaluate"
        ));
    }
    let path = if value == "~" {
        home.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(value)
    };
    if !path.is_absolute() {
        return Err(format!(
            "npm cache setting `{value}` is not an absolute path"
        ));
    }
    Ok(path)
}

/// Resolves npm's cache directory the way npm does for a user, without
/// running npm (which writes logs and may contact the registry):
/// `npm_config_cache` environment variable, then the user npmrc
/// (`$NPM_CONFIG_USERCONFIG` or `~/.npmrc`), then `~/.npm`.
/// Project-level and global npmrc files are not consulted.
pub fn detect_npm_cache_dir(
    home: &Path,
    env: impl Fn(&str) -> Option<String>,
) -> std::result::Result<(PathBuf, String), String> {
    for var in ["npm_config_cache", "NPM_CONFIG_CACHE"] {
        if let Some(v) = env(var).filter(|v| !v.is_empty()) {
            return npm_cache_value_to_path(&v, home).map(|p| (p, format!("${var}")));
        }
    }
    let userconfig = ["npm_config_userconfig", "NPM_CONFIG_USERCONFIG"]
        .iter()
        .find_map(|v| env(v).filter(|v| !v.is_empty()))
        .map(|v| npm_cache_value_to_path(&v, home))
        .transpose()?
        .unwrap_or_else(|| home.join(".npmrc"));
    match std::fs::read_to_string(&userconfig) {
        Ok(text) => {
            if let Some(v) = npmrc_cache_value(&text) {
                return npm_cache_value_to_path(&v, home)
                    .map(|p| (p, format!("cache= in {}", userconfig.display())));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot read {}: {e}", userconfig.display())),
    }
    Ok((home.join(".npm"), "npm default ~/.npm".to_string()))
}

fn xcode_is_running() -> bool {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    ["Xcode", "xcodebuild", "XCBBuildService", "SWBBuildService"]
        .iter()
        .any(|n| sys.processes_by_exact_name(OsStr::new(n)).next().is_some())
}

/// Entry points of npm, npx, yarn and pnpm, either as a process name
/// (standalone binaries) or as the script a Node.js process runs
/// (`node /opt/homebrew/bin/npm`, `node …/npm/bin/npm-cli.js`, corepack's
/// `…/dist/yarn.js`).
const PACKAGE_MANAGER_ENTRY_POINTS: [&str; 15] = [
    "npm",
    "npx",
    "npm-cli.js",
    "npx-cli.js",
    "yarn",
    "yarnpkg",
    "yarn.js",
    "yarn.cjs",
    "pnpm",
    "pnpx",
    "pnpm.cjs",
    "pnpm.mjs",
    "pnpx.cjs",
    "pnpm.js",
    "pnpx.js",
];

/// True when a process with this name and command line is npm, npx, yarn
/// or pnpm.
fn is_package_manager_process(name: &OsStr, cmd: &[OsString]) -> bool {
    let is_entry = |s: &OsStr| {
        Path::new(s)
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|s| PACKAGE_MANAGER_ENTRY_POINTS.contains(&s))
    };
    if is_entry(name) {
        return true;
    }
    // npm and others set `process.title`, which Node.js writes over argv:
    // `npm install` then shows up as argv[0] "npm install" and no other
    // arguments.
    if let Some(first) = cmd.first()
        && let Some(word) = first.to_string_lossy().split_whitespace().next()
        && is_entry(OsStr::new(word))
    {
        return true;
    }
    if !matches!(name.to_str(), Some("node" | "nodejs")) {
        return false;
    }
    // The script is the first argument after node's own options.
    cmd.iter()
        .skip(1)
        .find(|a| !a.to_string_lossy().starts_with('-'))
        .is_some_and(|script| is_entry(script))
}

/// How many running package-manager processes a refusal names.
const MAX_LISTED_PROCESSES: usize = 3;

/// Describes the running npm, npx, yarn or pnpm processes, if any.
fn running_package_manager() -> Option<String> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    let mut found: Vec<_> = sys
        .processes()
        .iter()
        .filter(|(_, p)| is_package_manager_process(p.name(), p.cmd()))
        .map(|(pid, p)| {
            let title = p
                .cmd()
                .first()
                .map_or_else(|| p.name().to_string_lossy(), |a| a.to_string_lossy());
            (pid.as_u32(), title.trim().to_string())
        })
        .collect();
    if found.is_empty() {
        return None;
    }
    found.sort();
    let mut text = found
        .iter()
        .take(MAX_LISTED_PROCESSES)
        .map(|(pid, title)| format!("process {pid} `{title}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if found.len() > MAX_LISTED_PROCESSES {
        text += &format!(" and {} more", found.len() - MAX_LISTED_PROCESSES);
    }
    Some(text)
}

/// Accepts `root` only when it exists as a real directory (not a symlink)
/// whose canonical path passes the policy.
fn finish_resolution(root: PathBuf, source: String, home: &Path) -> Resolution {
    match std::fs::symlink_metadata(&root) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Resolution::NotPresent(format!("{} does not exist", root.display()));
        }
        Err(e) => return Resolution::Refused(format!("cannot inspect {}: {e}", root.display())),
        Ok(m) if m.file_type().is_symlink() => {
            return Resolution::Refused(format!(
                "{} is a symbolic link; linked cache locations are not supported",
                root.display()
            ));
        }
        Ok(m) if !m.is_dir() => {
            return Resolution::Refused(format!("{} is not a directory", root.display()));
        }
        Ok(_) => {}
    }
    let canonical = match root.canonicalize() {
        Ok(c) => c,
        Err(e) => return Resolution::Refused(format!("cannot resolve {}: {e}", root.display())),
    };
    if let Err(why) = policy::validate_category_root(&canonical, home) {
        return Resolution::Refused(why);
    }
    Resolution::Root {
        path: canonical,
        source,
    }
}

fn resolve_root(id: CategoryId, ctx: &CleanContext) -> Resolution {
    match id {
        CategoryId::NpmCache => {
            let running = match ctx.package_manager_running {
                Some(true) => Some("a package manager".to_string()),
                Some(false) => None,
                None => running_package_manager(),
            };
            if let Some(what) = running {
                return Resolution::Refused(format!(
                    "npm, npx, yarn or pnpm is running ({what}); wait for it to finish or \
                     quit it before cleaning the npm cache"
                ));
            }
            let (dir, source) = match &ctx.npm_cache {
                NpmCacheSource::Explicit(p) => (p.clone(), "configured".to_string()),
                NpmCacheSource::Detect => {
                    match detect_npm_cache_dir(&ctx.home, |k| {
                        std::env::var_os(k).map(|v| v.to_string_lossy().into_owned())
                    }) {
                        Ok(found) => found,
                        Err(why) => return Resolution::Refused(why),
                    }
                }
            };
            if !dir.is_absolute() {
                return Resolution::Refused(format!(
                    "npm cache path {} is not absolute",
                    dir.display()
                ));
            }
            finish_resolution(dir.join("_cacache"), source, &ctx.home)
        }
        CategoryId::XcodeDerivedData => {
            let running = ctx.xcode_running.unwrap_or_else(xcode_is_running);
            if running {
                return Resolution::Refused(
                    "Xcode or xcodebuild is running; quit it before cleaning DerivedData".into(),
                );
            }
            let expected = ctx.home.join("Library/Developer/Xcode/DerivedData");
            match finish_resolution(expected.clone(), "Xcode default location".into(), &ctx.home) {
                Resolution::Root { path, .. } if path != expected => Resolution::Refused(format!(
                    "{} resolves to {}; relocated DerivedData is not supported",
                    expected.display(),
                    path.display()
                )),
                other => other,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

const NPM_ENTRIES: [&str; 3] = ["content-v2", "index-v5", "tmp"];
const XCODE_CACHE_DIRS: [&str; 3] = [
    "ModuleCache.noindex",
    "SymbolCache.noindex",
    "CompilationCache.noindex",
];

/// Xcode names per-project folders `<Project>-<28 lowercase letters>`.
fn is_xcode_project_dir_name(name: &str) -> bool {
    match name.rsplit_once('-') {
        Some((project, hash)) => {
            !project.is_empty() && hash.len() == 28 && hash.bytes().all(|b| b.is_ascii_lowercase())
        }
        None => false,
    }
}

fn classify(id: CategoryId, name: &OsStr, meta: &Meta, uid: u32) -> Verdict {
    if meta.uid != uid {
        return Verdict::forbidden("owned by another user");
    }
    if meta.kind == FileKind::Symlink {
        return Verdict::forbidden("symbolic link; links are never followed");
    }
    let Some(name) = name.to_str() else {
        return Verdict::review("name is not valid UTF-8");
    };
    match id {
        CategoryId::NpmCache => {
            if meta.kind == FileKind::Dir && NPM_ENTRIES.contains(&name) {
                Verdict::safe(format!("npm cacache `{name}` directory"))
            } else {
                Verdict::review("not part of npm's cache layout; left untouched")
            }
        }
        CategoryId::XcodeDerivedData => {
            if meta.kind != FileKind::Dir {
                Verdict::review("not a directory; left untouched")
            } else if XCODE_CACHE_DIRS.contains(&name) {
                Verdict::safe(format!("Xcode cache `{name}`"))
            } else if is_xcode_project_dir_name(name) {
                let project = name.rsplit_once('-').map(|(p, _)| p).unwrap_or(name);
                Verdict::safe(format!("build data for Xcode project `{project}`"))
            } else {
                Verdict::review("not a recognized DerivedData folder; left untouched")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Descriptor-relative walker shared by measurement and deletion
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Measure,
    Delete,
}

struct Walker<'t> {
    mode: Mode,
    uid: u32,
    cutoff_ns: i128,
    dev: u64,
    tally: &'t mut Tally,
}

impl Walker<'_> {
    /// Why a non-directory entry may not be removed, if it may not.
    fn ineligible(&self, m: &Meta) -> Option<&'static str> {
        if m.uid != self.uid {
            Some("owned by another user")
        } else if m.kind == FileKind::File && m.nlink > 1 {
            Some("has other hard links; space would not be reclaimed")
        } else if m.changed_ns > self.cutoff_ns {
            Some("modified after the cleanup plan was created")
        } else if m.is_immutable_or_append_only() {
            Some("immutable or append-only flag is set")
        } else {
            None
        }
    }

    /// Processes one entry of `parent`. Returns true when the entry is gone
    /// (or would be gone, when measuring).
    fn entry(
        &mut self,
        parent: &std::os::fd::OwnedFd,
        name: &OsStr,
        path: &Path,
        depth: usize,
    ) -> bool {
        let m = match fsops::stat_at(borrow(parent), name) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.tally
                    .skip(path, "disappeared before it could be removed");
                return true;
            }
            Err(e) => {
                self.tally.error(path, &e);
                return false;
            }
        };
        match m.kind {
            FileKind::Dir => self.dir(parent, name, path, m, depth),
            FileKind::File | FileKind::Symlink => {
                if let Some(why) = self.ineligible(&m) {
                    self.tally.skip(path, why);
                    return false;
                }
                if self.mode == Mode::Delete {
                    match fsops::unlink_at(borrow(parent), name) {
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {
                            self.tally
                                .skip(path, "disappeared before it could be removed");
                            return true;
                        }
                        Err(e) => {
                            self.tally.error(path, &e);
                            return false;
                        }
                    }
                }
                if m.kind == FileKind::File {
                    self.tally.files += 1;
                    self.tally.logical_bytes += m.logical_bytes;
                    self.tally.allocated_bytes += m.allocated_bytes;
                } else {
                    self.tally.symlinks += 1;
                }
                true
            }
            FileKind::Other => {
                self.tally
                    .skip(path, "special file (socket, FIFO or device)");
                false
            }
        }
    }

    fn dir(
        &mut self,
        parent: &std::os::fd::OwnedFd,
        name: &OsStr,
        path: &Path,
        expected: Meta,
        depth: usize,
    ) -> bool {
        if expected.uid != self.uid {
            self.tally.skip(path, "owned by another user");
            return false;
        }
        if expected.id.dev != self.dev {
            self.tally.skip(path, "mount point of another filesystem");
            return false;
        }
        if depth >= MAX_DEPTH {
            self.tally.skip(path, "nested too deeply");
            return false;
        }
        let fd = match fsops::open_dir_at(borrow(parent), name) {
            Ok(fd) => fd,
            Err(e) => {
                self.tally.error(path, &e);
                return false;
            }
        };
        match fsops::fstat(borrow(&fd)) {
            Ok(m) if m.id == expected.id => {}
            Ok(_) => {
                self.tally.skip(path, "replaced while being examined");
                return false;
            }
            Err(e) => {
                self.tally.error(path, &e);
                return false;
            }
        }
        let names = match fsops::read_dir_names(borrow(&fd)) {
            Ok(n) => n,
            Err(e) => {
                self.tally.error(path, &e);
                return false;
            }
        };
        let mut all_gone = true;
        for child in names {
            let child_path = path.join(&child);
            all_gone &= self.entry(&fd, &child, &child_path, depth + 1);
        }
        drop(fd);
        if !all_gone {
            return false;
        }
        if self.mode == Mode::Delete {
            // Only remove the directory if the name still refers to it.
            match fsops::stat_at(borrow(parent), name) {
                Ok(m) if m.id == expected.id => {}
                Ok(_) => {
                    self.tally.skip(path, "replaced during cleanup");
                    return false;
                }
                Err(e) => {
                    self.tally.error(path, &e);
                    return false;
                }
            }
            if let Err(e) = fsops::rmdir_at(borrow(parent), name) {
                if e.raw_os_error() == Some(libc::ENOTEMPTY) {
                    self.tally.skip(path, "new entries appeared during cleanup");
                } else {
                    self.tally.error(path, &e);
                }
                return false;
            }
        }
        self.tally.directories += 1;
        true
    }
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

fn plan_category(id: CategoryId, ctx: &CleanContext, cutoff_ns: i128) -> CategoryPlan {
    let mut plan = CategoryPlan {
        category: id,
        description: id.description(),
        state: CategoryState::Empty,
        state_reason: None,
        root: None,
        root_source: None,
        root_id: None,
        candidates: Vec::new(),
        not_eligible: Vec::new(),
        estimate: Tally::default(),
    };
    let (root, source) = match resolve_root(id, ctx) {
        Resolution::Root { path, source } => (path, source),
        Resolution::NotPresent(why) => {
            plan.state = CategoryState::NotPresent;
            plan.state_reason = Some(why);
            return plan;
        }
        Resolution::Refused(why) => {
            plan.state = CategoryState::Refused;
            plan.state_reason = Some(why);
            return plan;
        }
    };
    plan.root = Some(root.clone());
    plan.root_source = Some(source);

    let refuse = |plan: &mut CategoryPlan, why: String| {
        plan.state = CategoryState::Refused;
        plan.state_reason = Some(why);
    };
    let root_fd = match fsops::open_dir_nofollow(&root) {
        Ok(fd) => fd,
        Err(e) => {
            refuse(&mut plan, format!("cannot open {}: {e}", root.display()));
            return plan;
        }
    };
    let root_meta = match fsops::fstat(borrow(&root_fd)) {
        Ok(m) => m,
        Err(e) => {
            refuse(&mut plan, format!("cannot inspect {}: {e}", root.display()));
            return plan;
        }
    };
    if root_meta.uid != ctx.uid {
        refuse(
            &mut plan,
            format!("{} is owned by another user", root.display()),
        );
        return plan;
    }
    plan.root_id = Some(root_meta.id);

    let mut names = match fsops::read_dir_names(borrow(&root_fd)) {
        Ok(n) => n,
        Err(e) => {
            refuse(&mut plan, format!("cannot list {}: {e}", root.display()));
            return plan;
        }
    };
    names.sort();

    if id == CategoryId::NpmCache && !names.iter().any(|n| n == "content-v2" || n == "index-v5") {
        if !names.is_empty() {
            refuse(
                &mut plan,
                format!("{} does not look like an npm cache", root.display()),
            );
            for n in &names {
                plan.not_eligible.push(ReviewItem {
                    path: root.join(n),
                    risk: Risk::Review,
                    reason: "npm cache layout not recognized".into(),
                });
            }
        }
        return plan;
    }

    for name in names {
        let path = root.join(&name);
        let meta = match fsops::stat_at(borrow(&root_fd), &name) {
            Ok(m) => m,
            Err(e) => {
                plan.not_eligible.push(ReviewItem {
                    path,
                    risk: Risk::Review,
                    reason: format!("cannot inspect: {e}"),
                });
                continue;
            }
        };
        let verdict = classify(id, &name, &meta, ctx.uid);
        if verdict.risk != Risk::Safe {
            plan.not_eligible.push(ReviewItem {
                path,
                risk: verdict.risk,
                reason: verdict.reason,
            });
            continue;
        }
        let mut estimate = Tally::default();
        Walker {
            mode: Mode::Measure,
            uid: ctx.uid,
            cutoff_ns,
            dev: root_meta.id.dev,
            tally: &mut estimate,
        }
        .entry(&root_fd, &name, &path, 0);
        plan.estimate.absorb(&estimate);
        plan.candidates.push(Candidate {
            name,
            path,
            kind: meta.kind,
            id: meta.id,
            reason: verdict.reason,
            estimate,
        });
    }
    plan.state = if plan.candidates.is_empty() {
        CategoryState::Empty
    } else {
        CategoryState::Ready
    };
    plan
}

/// Builds a read-only cleanup plan. Never modifies the filesystem.
pub fn plan(categories: &[CategoryId], ctx: &CleanContext) -> CleanPlan {
    let created_ns = now_ns();
    CleanPlan {
        dry_run: true,
        created_unix_secs: (created_ns / 1_000_000_000) as u64,
        created_ns,
        categories: categories
            .iter()
            .map(|&id| plan_category(id, ctx, created_ns))
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

fn available_bytes(path: &Path) -> Option<u64> {
    platform::volume_stats(path)
        .ok()
        .map(|v: VolumeStats| v.available_bytes)
}

fn execute_category(cat: &CategoryPlan, ctx: &CleanContext, cutoff_ns: i128) -> CategoryOutcome {
    let mut out = CategoryOutcome {
        category: cat.category,
        refused: None,
        rejected_candidates: Vec::new(),
        removed: Tally::default(),
        observed_available_delta_bytes: None,
    };
    let (Some(planned_root), Some(planned_id)) = (&cat.root, cat.root_id) else {
        out.refused = Some("category had no verified root in the plan".into());
        return out;
    };

    // Re-resolve from scratch: configuration or the filesystem may have changed.
    let root = match resolve_root(cat.category, ctx) {
        Resolution::Root { path, .. } if &path == planned_root => path,
        Resolution::Root { path, .. } => {
            out.refused = Some(format!(
                "category root changed from {} to {}",
                planned_root.display(),
                path.display()
            ));
            return out;
        }
        Resolution::NotPresent(why) | Resolution::Refused(why) => {
            out.refused = Some(why);
            return out;
        }
    };
    let root_fd = match fsops::open_dir_nofollow(&root) {
        Ok(fd) => fd,
        Err(e) => {
            out.refused = Some(format!("cannot open {}: {e}", root.display()));
            return out;
        }
    };
    match fsops::fstat(borrow(&root_fd)) {
        Ok(m) if m.id == planned_id && m.uid == ctx.uid => {}
        Ok(_) => {
            out.refused = Some(format!(
                "{} is no longer the directory that was planned",
                root.display()
            ));
            return out;
        }
        Err(e) => {
            out.refused = Some(format!("cannot inspect {}: {e}", root.display()));
            return out;
        }
    }

    let before = available_bytes(&root);
    for cand in &cat.candidates {
        let reject = |out: &mut CategoryOutcome, why: String| {
            out.rejected_candidates.push(ItemRecord {
                path: cand.path.clone(),
                reason: why,
            });
        };
        if fsops::check_entry_name(&cand.name).is_err() || cand.path != root.join(&cand.name) {
            reject(
                &mut out,
                "candidate is not a direct child of the root".into(),
            );
            continue;
        }
        let meta = match fsops::stat_at(borrow(&root_fd), &cand.name) {
            Ok(m) => m,
            Err(e) => {
                reject(&mut out, format!("cannot revalidate: {e}"));
                continue;
            }
        };
        if meta.id != cand.id || meta.kind != cand.kind {
            reject(&mut out, "replaced since the plan was created".into());
            continue;
        }
        let verdict = classify(cat.category, &cand.name, &meta, ctx.uid);
        if verdict.risk != Risk::Safe {
            reject(&mut out, verdict.reason);
            continue;
        }
        Walker {
            mode: Mode::Delete,
            uid: ctx.uid,
            cutoff_ns,
            dev: planned_id.dev,
            tally: &mut out.removed,
        }
        .entry(&root_fd, &cand.name, &cand.path, 0);
    }
    if let (Some(b), Some(a)) = (before, available_bytes(&root)) {
        out.observed_available_delta_bytes = Some(i128::from(a) - i128::from(b));
    }
    out
}

/// Deletes the candidates of a plan after revalidating each of them.
/// The caller is responsible for obtaining the user's confirmation first.
pub fn execute(plan: &CleanPlan, ctx: &CleanContext) -> CleanOutcome {
    CleanOutcome {
        categories: plan
            .categories
            .iter()
            .filter(|c| c.state == CategoryState::Ready)
            .map(|c| execute_category(c, ctx, plan.created_ns))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_cache_detection_order() {
        let home = Path::new("/Users/ann");
        let none = |_: &str| None;
        assert_eq!(
            detect_npm_cache_dir(home, none).unwrap().0,
            home.join(".npm")
        );
        let env = |k: &str| (k == "npm_config_cache").then(|| "~/caches/npm".to_string());
        assert_eq!(
            detect_npm_cache_dir(home, env).unwrap().0,
            home.join("caches/npm")
        );
        let rel = |k: &str| (k == "npm_config_cache").then(|| "relative/npm".to_string());
        assert!(detect_npm_cache_dir(home, rel).is_err());
        let var = |k: &str| (k == "NPM_CONFIG_CACHE").then(|| "${HOME}/x".to_string());
        assert!(detect_npm_cache_dir(home, var).is_err());
    }

    #[test]
    fn npmrc_parsing() {
        assert_eq!(
            npmrc_cache_value("registry=x\ncache=/a/b\n"),
            Some("/a/b".into())
        );
        assert_eq!(
            npmrc_cache_value("cache = \"/a b\"\n; cache=/no\n"),
            Some("/a b".into())
        );
        assert_eq!(
            npmrc_cache_value("cache=/one\nCACHE=/two"),
            Some("/two".into())
        );
        assert_eq!(npmrc_cache_value("# cache=/x\ncache-min=10"), None);
    }

    #[test]
    fn npmrc_file_is_used() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join(".npmrc"), "cache=~/custom-npm\n").unwrap();
        let (dir, source) = detect_npm_cache_dir(t.path(), |_| None).unwrap();
        assert_eq!(dir, t.path().join("custom-npm"));
        assert!(source.contains(".npmrc"));
    }

    #[test]
    fn package_manager_processes() {
        let is_pm = |name: &str, cmd: &[&str]| {
            let cmd: Vec<OsString> = cmd.iter().map(OsString::from).collect();
            is_package_manager_process(OsStr::new(name), &cmd)
        };
        assert!(is_pm("node", &["node", "/opt/homebrew/bin/npm", "install"]));
        assert!(is_pm(
            "node",
            &[
                "node",
                "--max-old-space-size=4096",
                "/x/npm/bin/npm-cli.js",
                "ci"
            ]
        ));
        assert!(is_pm("node", &["node", "/usr/local/bin/npx", "create-app"]));
        assert!(is_pm("nodejs", &["nodejs", "/x/corepack/dist/yarn.js"]));
        assert!(is_pm("node", &["node", "/x/pnpm/bin/pnpm.cjs", "add", "a"]));
        assert!(is_pm("pnpm", &["pnpm", "install"]));
        assert!(is_pm("yarn", &[]));
        // process.title rewrites argv: what `ps` shows for a running npm.
        assert!(is_pm("node", &["npm run wait"]));
        assert!(is_pm("node", &["npm exec chrome-devtools-mcp@latest", ""]));
        assert!(is_pm("node", &["pnpm"]));

        assert!(!is_pm("node", &["node", "server.js"]));
        assert!(!is_pm("node", &["node"]));
        assert!(!is_pm("node", &["node", "--", "app.js", "npm"]));
        assert!(!is_pm("vim", &["vim", "npm"]));
        assert!(!is_pm("npm-check", &["npm-check"]));
        assert!(!is_pm("zsh", &["/bin/zsh", "-c", "npm install"]));
        assert!(!is_pm("node", &["node server.js --npm"]));
    }

    #[test]
    fn xcode_project_dir_names() {
        assert!(is_xcode_project_dir_name(
            "MyApp-abcdefghijklmnopqrstuvwxyzab"
        ));
        assert!(is_xcode_project_dir_name(
            "My-App-abcdefghijklmnopqrstuvwxyzab"
        ));
        assert!(!is_xcode_project_dir_name("-abcdefghijklmnopqrstuvwxyzab"));
        assert!(!is_xcode_project_dir_name(
            "MyApp-ABCDEFGHIJKLMNOPQRSTUVWXYZAB"
        ));
        assert!(!is_xcode_project_dir_name("MyApp-short"));
        assert!(!is_xcode_project_dir_name("Documents"));
    }
}
