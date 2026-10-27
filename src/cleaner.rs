//! Cleanup planning (read-only) and guarded execution.
//!
//! Planning resolves each category root, validates it against the policy,
//! classifies the root's entries, and measures eligible candidates. It never
//! deletes anything. Execution re-resolves and re-opens the root, checks
//! that every candidate is still the same filesystem object, and then
//! deletes descriptor-relatively without following symbolic links.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::errors::{MacCleanError, Result};
use crate::npm_config;
use crate::platform::fsops::{self, FileId, FileKind, Meta, borrow};
use crate::platform::{self, PrefValue, VolumeStats};
use crate::policy::{self, Risk, Verdict};

/// How many skipped items and errors are listed individually per category.
pub const MAX_RECORDED_ITEMS: usize = 100;
/// Directory nesting below a candidate beyond which MacClean refuses to go.
pub const MAX_DEPTH: usize = 128;
/// Skip reason for entries that belong to another user (typically left by
/// `sudo npm`). Reports group these instead of listing each one.
pub const OWNED_BY_ANOTHER_USER: &str = "owned by another user";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum CategoryId {
    /// npm's content-addressable cache (`<npm cache>/_cacache`).
    NpmCache,
    /// Xcode build products and indexes in `~/Library/Developer/Xcode/DerivedData`.
    XcodeDerivedData,
    /// pip's download and wheel cache in `~/Library/Caches/pip`.
    PipCache,
}

impl CategoryId {
    pub const ALL: [CategoryId; 3] = [
        CategoryId::NpmCache,
        CategoryId::XcodeDerivedData,
        CategoryId::PipCache,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CategoryId::NpmCache => "npm-cache",
            CategoryId::XcodeDerivedData => "xcode-derived-data",
            CategoryId::PipCache => "pip-cache",
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
            CategoryId::PipCache => {
                "pip's HTTP and locally built wheel caches in ~/Library/Caches/pip. pip \
                 downloads or builds packages again when it needs them; equivalent to \
                 `pip cache purge`. Self-check state and unknown entries are not touched. \
                 Refused while pip is running."
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum NpmCacheSource {
    /// Read npm's configuration files and environment (see [`npm_config::detect_npm_cache_dir`]).
    Detect,
    /// Use this npm cache directory (the parent of `_cacache`).
    Explicit(PathBuf),
}

/// Where to get Xcode's DerivedData setting from.
#[derive(Debug, Clone)]
pub enum DerivedDataSetting {
    /// Read `IDECustomDerivedDataLocation` from the `com.apple.dt.Xcode`
    /// preferences of the current user.
    ReadPreferences,
    /// Use this value as if it had been read from the preferences.
    Given(PrefValue),
}

pub const XCODE_PREFS_DOMAIN: &str = "com.apple.dt.Xcode";
pub const XCODE_DERIVED_DATA_KEY: &str = "IDECustomDerivedDataLocation";

/// Turns Xcode's DerivedData setting into the directory to clean and a
/// description of where it came from, or explains why it is refused.
/// Xcode stores an absolute path for a custom location, a relative one
/// (resolved per workspace) for a relative location, and nothing for the
/// default.
pub fn derived_data_location(
    setting: &PrefValue,
    home: &Path,
) -> std::result::Result<(PathBuf, String), String> {
    let value = match setting {
        PrefValue::Missing => {
            return Ok((
                home.join("Library/Developer/Xcode/DerivedData"),
                "Xcode default location".into(),
            ));
        }
        PrefValue::NotAString => {
            return Err(format!(
                "Xcode setting {XCODE_DERIVED_DATA_KEY} is not a path; DerivedData location is unclear"
            ));
        }
        PrefValue::String(v) => v.trim(),
    };
    if value.is_empty() {
        return Ok((
            home.join("Library/Developer/Xcode/DerivedData"),
            "Xcode default location".into(),
        ));
    }
    let path = if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(value)
    };
    if !path.is_absolute() {
        return Err(format!(
            "Xcode keeps DerivedData relative to each workspace (`{value}`); MacClean cleans \
             only one fixed location"
        ));
    }
    if path
        .components()
        .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(format!(
            "Xcode DerivedData location `{value}` contains `..` components"
        ));
    }
    Ok((
        path,
        format!("Xcode custom location ({XCODE_DERIVED_DATA_KEY})"),
    ))
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
    /// `None` means detect running pip processes.
    pub pip_running: Option<bool>,
    pub derived_data: DerivedDataSetting,
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
            pip_running: test_package_manager_override(),
            derived_data: DerivedDataSetting::ReadPreferences,
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
    /// How many of the skipped entries belong to another user. Included in
    /// `skipped_total`.
    pub foreign_owned: u64,
    pub skipped: Vec<ItemRecord>,
    pub errors_total: u64,
    pub errors: Vec<ItemRecord>,
}

impl Tally {
    fn skip(&mut self, path: &Path, reason: impl Into<String>) {
        let reason = reason.into();
        self.skipped_total += 1;
        if reason == OWNED_BY_ANOTHER_USER {
            self.foreign_owned += 1;
        }
        if self.skipped.len() < MAX_RECORDED_ITEMS {
            self.skipped.push(ItemRecord {
                path: path.to_path_buf(),
                reason,
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
        self.foreign_owned += other.foreign_owned;
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
    /// Settings that were noticed but not used (for example a project npmrc).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
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
    /// The category root from the plan.
    pub root: Option<PathBuf>,
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
    Root {
        path: PathBuf,
        source: String,
        /// Settings that were noticed but not used, shown with the plan.
        notes: Vec<String>,
    },
    NotPresent(String),
    Refused(String),
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

/// True when a process with this name and command line is pip: a `pip`,
/// `pip3` or `pip3.12` script (run by Python, so the script is an argument),
/// or `python -m pip`.
fn is_pip_process(name: &OsStr, cmd: &[OsString]) -> bool {
    let is_pip_name = |s: &OsStr| {
        Path::new(s)
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|n| n.strip_prefix("pip"))
            .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit() || c == '.'))
    };
    if is_pip_name(name) {
        return true;
    }
    let is_python = name
        .to_str()
        .is_some_and(|n| n.to_ascii_lowercase().starts_with("python"));
    if !is_python {
        return false;
    }
    let args: Vec<_> = cmd.iter().skip(1).map(|a| a.to_string_lossy()).collect();
    // `python -m pip …` or `python -mpip …`
    if args.windows(2).any(|w| w[0] == "-m" && w[1] == "pip") || args.iter().any(|a| a == "-mpip") {
        return true;
    }
    // `python /usr/local/bin/pip3 install …`: the first non-option argument.
    args.iter()
        .find(|a| !a.starts_with('-'))
        .is_some_and(|script| is_pip_name(OsStr::new(script.as_ref())))
}

/// Describes the running pip processes, if any.
fn running_pip() -> Option<String> {
    running_processes(is_pip_process)
}

/// Describes the running npm, npx, yarn or pnpm processes, if any.
fn running_package_manager() -> Option<String> {
    running_processes(is_package_manager_process)
}

/// A process's command line for a message: the program's file name and
/// its arguments, joined by spaces (or its name if they are unreadable), at
/// most 120 characters.
fn command_line(name: &OsStr, cmd: &[OsString]) -> String {
    let joined = cmd
        .iter()
        .enumerate()
        .map(|(i, a)| match Path::new(a).file_name() {
            // argv[0] can be a long interpreter path; its name is enough.
            Some(file) if i == 0 && a.to_string_lossy().starts_with('/') => file.to_string_lossy(),
            _ => a.to_string_lossy(),
        })
        .filter(|a| !a.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let text = if joined.trim().is_empty() {
        name.to_string_lossy().into_owned()
    } else {
        joined.trim().to_string()
    };
    match text.char_indices().nth(120) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    }
}

/// Describes the processes `matches` accepts, if any: up to
/// `MAX_LISTED_PROCESSES` of them by PID and command line.
fn running_processes(matches: fn(&OsStr, &[OsString]) -> bool) -> Option<String> {
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
        .filter(|(_, p)| matches(p.name(), p.cmd()))
        .map(|(pid, p)| (pid.as_u32(), command_line(p.name(), p.cmd())))
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
        notes: Vec::new(),
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
            let (dir, source, notes) = match &ctx.npm_cache {
                NpmCacheSource::Explicit(p) => (p.clone(), "configured".to_string(), Vec::new()),
                NpmCacheSource::Detect => {
                    let cwd = std::env::current_dir().ok();
                    match npm_config::detect_npm_cache_dir(&ctx.home, cwd.as_deref(), |k| {
                        std::env::var_os(k).map(|v| v.to_string_lossy().into_owned())
                    }) {
                        Ok(found) => (found.dir, found.source, found.notes),
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
            match finish_resolution(dir.join("_cacache"), source, &ctx.home) {
                Resolution::Root { path, source, .. } => Resolution::Root {
                    path,
                    source,
                    notes,
                },
                other => other,
            }
        }
        CategoryId::XcodeDerivedData => {
            let running = ctx.xcode_running.unwrap_or_else(xcode_is_running);
            if running {
                return Resolution::Refused(
                    "Xcode or xcodebuild is running; quit it before cleaning DerivedData".into(),
                );
            }
            let setting = match &ctx.derived_data {
                DerivedDataSetting::ReadPreferences => {
                    platform::read_app_preference(XCODE_PREFS_DOMAIN, XCODE_DERIVED_DATA_KEY)
                }
                DerivedDataSetting::Given(v) => v.clone(),
            };
            let (expected, source) = match derived_data_location(&setting, &ctx.home) {
                Ok(found) => found,
                Err(why) => return Resolution::Refused(why),
            };
            // The same root policy applies to a custom location: inside home,
            // not protected, not a symlink, and not reached through one.
            match finish_resolution(expected.clone(), source, &ctx.home) {
                Resolution::Root { path, .. } if path != expected => Resolution::Refused(format!(
                    "{} resolves to {}; DerivedData reached through a symbolic link is not \
                     supported",
                    expected.display(),
                    path.display()
                )),
                other => other,
            }
        }
        CategoryId::PipCache => {
            let running = match ctx.pip_running {
                Some(true) => Some("a pip process".to_string()),
                Some(false) => None,
                None => running_pip(),
            };
            if let Some(what) = running {
                return Resolution::Refused(format!(
                    "pip is running ({what}); wait for it to finish before cleaning its cache"
                ));
            }
            // Only pip's default location is cleaned. Other cache-dir settings
            // depend on the Python that runs pip, so they are reported only.
            let notes = pip_cache_dir_settings(&ctx.home, |k| {
                std::env::var_os(k).map(|v| v.to_string_lossy().into_owned())
            });
            let expected = ctx.home.join("Library/Caches/pip");
            match finish_resolution(expected.clone(), "pip default location".into(), &ctx.home) {
                Resolution::Root { path, .. } if path != expected => Resolution::Refused(format!(
                    "{} resolves to {}; a pip cache reached through a symbolic link is not \
                     supported",
                    expected.display(),
                    path.display()
                )),
                Resolution::Root { path, source, .. } => Resolution::Root {
                    path,
                    source,
                    notes,
                },
                other => other,
            }
        }
    }
}

/// Other cache locations pip may be configured with: `PIP_CACHE_DIR` and
/// `cache-dir` in the config files pip reads for every Python (the file in
/// `PIP_CONFIG_FILE`, the global file, and the user files). Each one found
/// becomes a note; MacClean does not follow them.
fn pip_cache_dir_settings(home: &Path, env: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut notes = Vec::new();
    let note = |where_: String, value: &str| {
        format!("{where_} sets pip's cache to `{value}`; MacClean cleans only ~/Library/Caches/pip")
    };
    if let Some(v) = env("PIP_CACHE_DIR").filter(|v| !v.is_empty()) {
        notes.push(note("$PIP_CACHE_DIR".into(), &v));
    }
    let mut files: Vec<PathBuf> = env("PIP_CONFIG_FILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .collect();
    files.extend([
        PathBuf::from("/Library/Application Support/pip/pip.conf"),
        home.join(".pip/pip.conf"),
        home.join(".config/pip/pip.conf"),
        home.join("Library/Application Support/pip/pip.conf"),
    ]);
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for key in ["cache-dir", "cache_dir"] {
            if let Some(v) = npm_config::npmrc_value(&text, key) {
                notes.push(note(file.display().to_string(), &v));
            }
        }
    }
    notes
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

const NPM_ENTRIES: [&str; 3] = ["content-v2", "index-v5", "tmp"];
/// What `pip cache purge` empties: the HTTP caches (`http-v2` since pip
/// 23.3, `http` before) and locally built wheels.
const PIP_ENTRIES: [&str; 3] = ["http", "http-v2", "wheels"];
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
        return Verdict::forbidden(OWNED_BY_ANOTHER_USER);
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
        CategoryId::PipCache => {
            if meta.kind == FileKind::Dir && PIP_ENTRIES.contains(&name) {
                Verdict::safe(format!("pip cache `{name}` directory"))
            } else {
                Verdict::review("not part of pip's cache layout; left untouched")
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
            Some(OWNED_BY_ANOTHER_USER)
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
            self.tally.skip(path, OWNED_BY_ANOTHER_USER);
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
        notes: Vec::new(),
        estimate: Tally::default(),
    };
    let (root, source) = match resolve_root(id, ctx) {
        Resolution::Root {
            path,
            source,
            notes,
        } => {
            plan.notes = notes;
            (path, source)
        }
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

    // A root holding none of the tool's own entries is not trusted at all.
    let signature: &[&str] = match id {
        CategoryId::NpmCache => &["content-v2", "index-v5"],
        CategoryId::PipCache => &PIP_ENTRIES,
        CategoryId::XcodeDerivedData => &[],
    };
    if !signature.is_empty() && !names.iter().any(|n| signature.iter().any(|s| n == s)) {
        if !names.is_empty() {
            refuse(
                &mut plan,
                format!(
                    "{} does not look like a {} directory",
                    root.display(),
                    id.name()
                ),
            );
            for n in &names {
                plan.not_eligible.push(ReviewItem {
                    path: root.join(n),
                    risk: Risk::Review,
                    reason: format!("{} layout not recognized", id.name()),
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
        root: cat.root.clone(),
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
    fn derived_data_setting_values() {
        let home = Path::new("/Users/ann");
        let default = home.join("Library/Developer/Xcode/DerivedData");
        let loc = |v: PrefValue| derived_data_location(&v, home);
        let s = |v: &str| PrefValue::String(v.to_string());

        assert_eq!(loc(PrefValue::Missing).unwrap().0, default);
        assert_eq!(loc(s("")).unwrap().0, default);
        let (path, source) = loc(s("/Users/ann/Builds/DD")).unwrap();
        assert_eq!(path, home.join("Builds/DD"));
        assert!(source.contains("custom"));
        assert_eq!(loc(s("~/DD/")).unwrap().0, home.join("DD"));

        assert!(loc(s("DerivedData")).unwrap_err().contains("relative"));
        assert!(loc(s("../DD")).is_err());
        assert!(loc(s("/Users/ann/x/../Documents")).is_err());
        // An interior `.` is dropped by Path itself and names the same directory.
        assert_eq!(loc(s("/Users/ann/./DD")).unwrap().0, home.join("DD"));
        assert!(loc(PrefValue::NotAString).is_err());
    }

    #[test]
    fn missing_preference_reads_as_missing() {
        assert_eq!(
            platform::read_app_preference(XCODE_PREFS_DOMAIN, "MacCleanNoSuchKey-7f3a"),
            PrefValue::Missing
        );
    }

    #[test]
    fn command_lines_for_messages() {
        let os = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            command_line(OsStr::new("node"), &os(&["npm install", "", ""])),
            "npm install"
        );
        assert_eq!(
            command_line(
                OsStr::new("Python"),
                &os(&["/x/Python", "/y/pip3", "install", "z"])
            ),
            "Python /y/pip3 install z"
        );
        assert_eq!(command_line(OsStr::new("pip"), &[]), "pip");
        let long = "a".repeat(300);
        assert_eq!(
            command_line(OsStr::new("x"), &os(&[&long])).chars().count(),
            121
        );
    }

    #[test]
    fn pip_processes() {
        let is_pip = |name: &str, cmd: &[&str]| {
            let cmd: Vec<OsString> = cmd.iter().map(OsString::from).collect();
            is_pip_process(OsStr::new(name), &cmd)
        };
        assert!(is_pip("python3", &["python3", "-m", "pip", "install", "x"]));
        assert!(is_pip("Python", &["/usr/bin/python3", "-mpip", "download"]));
        assert!(is_pip(
            "python3.12",
            &["python3.12", "/opt/homebrew/bin/pip3", "install"]
        ));
        assert!(is_pip(
            "python3",
            &["python3", "-u", "/x/bin/pip3.12", "wheel", "."]
        ));
        assert!(is_pip("pip", &["pip", "install"]));
        assert!(is_pip("pip3", &[]));

        assert!(!is_pip("python3", &["python3", "-m", "pipx", "run"]));
        assert!(!is_pip("python3", &["python3", "app.py", "pip"]));
        assert!(!is_pip("python3", &["python3", "/x/pipeline.py"]));
        assert!(!is_pip("pipx", &["pipx", "install"]));
        assert!(!is_pip("node", &["node", "pip"]));
    }

    #[test]
    fn pip_cache_dir_settings_from_env_and_files() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        let custom = home.join("custom.conf");
        std::fs::write(&custom, "[global]\ncache_dir = /from/custom\n").unwrap();
        std::fs::create_dir_all(home.join(".pip")).unwrap();
        std::fs::write(
            home.join(".pip/pip.conf"),
            "[install]\ncache-dir=/from/legacy\n",
        )
        .unwrap();
        let env = |k: &str| match k {
            "PIP_CACHE_DIR" => Some("/from/env".to_string()),
            "PIP_CONFIG_FILE" => Some(custom.display().to_string()),
            _ => None,
        };
        let notes = pip_cache_dir_settings(home, env);
        for v in ["/from/env", "/from/custom", "/from/legacy"] {
            assert!(notes.iter().any(|n| n.contains(v)), "{v}: {notes:?}");
        }
        assert!(
            pip_cache_dir_settings(home, |_| None)
                .iter()
                .all(|n| !n.contains("/from/env"))
        );
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
