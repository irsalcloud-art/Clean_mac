//! Property, fuzz-style and stress tests for the parts of MacClean that
//! decide what may be deleted. Every filesystem test works inside temporary
//! directories and checks that nothing outside them changed.
//!
//! The number of generated cases is bounded so the suite stays fast in CI:
//! proptest runs 256 cases per property (`PROPTEST_CASES` overrides it) and
//! the stress test runs `MACCLEAN_STRESS_ROUNDS` rounds (default 20).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use macclean::cleaner::{
    self, ApprovedPlan, CategoryId, CategoryState, CleanContext, CleanPlan, DerivedDataSetting,
    NpmCacheSource,
};
use macclean::platform::{self, PrefValue, fsops};
use macclean::policy::validate_category_root;
use proptest::prelude::*;

/// Approves a copy of `plan` the way the CLI does after the user types `yes`.
fn approve(plan: &CleanPlan) -> ApprovedPlan {
    plan.clone().approve(cleaner::CONFIRMATION_WORD).unwrap()
}

// ---------------------------------------------------------------------------
// Snapshots: what a directory tree looks like, without following symlinks
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Node {
    Dir,
    File(Vec<u8>),
    Link(PathBuf),
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Node> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            let m = fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(root).unwrap().to_path_buf();
            let node = if m.file_type().is_symlink() {
                Node::Link(fs::read_link(&p).unwrap())
            } else if m.is_dir() {
                stack.push(p.clone());
                Node::Dir
            } else {
                Node::File(fs::read(&p).unwrap())
            };
            out.insert(rel, node);
        }
    }
    out
}

fn write(p: &Path, data: &[u8]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

// ---------------------------------------------------------------------------
// policy::validate_category_root
// ---------------------------------------------------------------------------

/// Path components that matter to the policy, plus random ones.
fn component() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[a-zA-Z0-9._ -]{1,10}",
        1 => Just(".".to_string()),
        1 => Just("..".to_string()),
        1 => Just(String::new()),
        3 => prop::sample::select(vec![
            "Library", "Documents", "Desktop", "Pictures", "Downloads", "Caches",
            "Developer", "Xcode", "Archives", "CoreSimulator", "Application Support",
            "Keychains", "Containers", ".ssh", ".git", ".config", ".npm", "_cacache",
            "DerivedData", "Users", "System", "Volumes",
        ])
        .prop_map(str::to_string),
        // Unicode look-alikes: decomposed accents, zero-width space,
        // fullwidth letters, case changes.
        2 => prop::sample::select(vec![
            "e\u{301}", "\u{e9}", "\u{200b}", "\u{ff24}ocuments", "documents", "LIBRARY",
            "Library\u{200b}", ".SSH",
        ])
        .prop_map(str::to_string),
    ]
}

/// A home directory such as `/Users/ann` and a candidate root built from a
/// base (home, a look-alike of home, or a system location) plus components.
fn home_and_root() -> impl Strategy<Value = (PathBuf, String)> {
    let name = "[a-z]{1,8}";
    let base = prop_oneof![
        4 => Just(0u8),
        2 => Just(1u8),
        1 => Just(2u8),
    ];
    let suffix = prop::sample::select(vec!["2", "_", ".bak", "\u{301}", " ", "-old"]);
    let system = prop::sample::select(vec!["/", "/Users", "/Volumes/Ext", "/private/var", "/tmp"]);
    (
        name,
        base,
        suffix,
        system,
        prop::collection::vec(component(), 0..6),
    )
        .prop_map(|(name, base, suffix, system, comps)| {
            let home = PathBuf::from(format!("/Users/{name}"));
            let mut root = match base {
                0 => home.display().to_string(),
                1 => format!("/Users/{name}{suffix}"),
                _ => system.trim_end_matches('/').to_string(),
            };
            for c in comps {
                root.push('/');
                root.push_str(&c);
            }
            if root.is_empty() {
                root.push('/');
            }
            (home, root)
        })
}

const PROTECTED_UNDER_HOME: [&str; 12] = [
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
];

const SENSITIVE_UNDER_HOME: [&str; 12] = [
    "Library/Keychains",
    "Library/Application Support",
    "Library/Containers",
    "Library/Mail",
    "Library/Preferences",
    "Library/Developer/CoreSimulator",
    "Library/Developer/Xcode/Archives",
    ".ssh",
    ".git",
    ".config",
    "Documents",
    "Desktop",
];

proptest! {
    /// Whatever the policy accepts is strictly inside home (by components,
    /// so `/Users/ann2` is never inside `/Users/ann`), has no `..`, and
    /// neither is nor contains a protected folder, nor lies in a sensitive one.
    #[test]
    fn accepted_roots_are_strictly_inside_home_and_unprotected((home, raw) in home_and_root()) {
        let root = Path::new(&raw);
        let verdict = validate_category_root(root, &home);
        if raw.split('/').any(|s| s == "..") {
            prop_assert!(verdict.is_err(), "`..` accepted: {raw}");
        }
        if !root.starts_with(&home) {
            prop_assert!(verdict.is_err(), "outside home accepted: {raw}");
        }
        if verdict.is_ok() {
            prop_assert!(root.is_absolute());
            prop_assert!(root
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_))));
            prop_assert!(root.starts_with(&home) && root != home);
            // The second component is exactly the home folder's name.
            prop_assert_eq!(
                root.components().nth(2),
                home.components().nth(2),
                "look-alike of home accepted: {}", raw
            );
            for p in PROTECTED_UNDER_HOME {
                let p = home.join(p);
                prop_assert!(root != p && !p.starts_with(root), "{raw} is or contains {}", p.display());
            }
            for s in SENSITIVE_UNDER_HOME {
                prop_assert!(!root.starts_with(home.join(s)), "{raw} is inside {s}");
            }
        }
    }

    /// Acceptance is hereditary: a plain subfolder of an accepted root is
    /// accepted too, so no protected location hides below an accepted root.
    #[test]
    fn accepted_roots_stay_accepted_below(
        (home, raw) in home_and_root(),
        child in "[a-zA-Z0-9._-]{1,10}",
    ) {
        prop_assume!(child != "." && child != "..");
        let root = Path::new(&raw);
        if validate_category_root(root, &home).is_ok() {
            prop_assert!(validate_category_root(&root.join(&child), &home).is_ok());
        }
    }
}

/// APFS is case- and normalization-insensitive while the policy compares
/// components exactly. Roots are canonicalized before the policy runs, and
/// canonicalization returns the on-disk spelling, so a differently spelled
/// path to a protected folder is still refused.
#[test]
fn differently_spelled_protected_folders_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    for dir in ["Documents", ".ssh", "Library/Application Support"] {
        write(
            &home.join(dir).join("_cacache/content-v2/blob"),
            b"precious",
        );
    }
    for spelling in [
        "documents",
        "DOCUMENTS",
        ".SSH",
        "library/application support",
    ] {
        let ctx = CleanContext {
            home: home.clone(),
            uid: platform::current_uid(),
            npm_cache: NpmCacheSource::Explicit(home.join(spelling)),
            xcode_running: Some(false),
            package_manager_running: Some(false),
            pip_running: Some(false),
            brew_running: Some(false),
            gradle_running: Some(false),
            pod_running: Some(false),
            derived_data: DerivedDataSetting::Given(PrefValue::Missing),
        };
        let plan = cleaner::plan(&[CategoryId::NpmCache], &ctx);
        assert_eq!(
            plan.categories[0].state,
            CategoryState::Refused,
            "{spelling}"
        );
        cleaner::execute(&approve(&plan), &ctx);
    }
    for dir in ["Documents", ".ssh", "Library/Application Support"] {
        assert!(home.join(dir).join("_cacache/content-v2/blob").exists());
    }
}

// ---------------------------------------------------------------------------
// fsops::check_entry_name and descriptor-relative operations
// ---------------------------------------------------------------------------

/// Byte strings biased towards the shapes that could escape a directory.
fn entry_name() -> impl Strategy<Value = Vec<u8>> {
    let piece = prop::sample::select(vec![
        &b"."[..],
        b"..",
        b"/",
        b"\0",
        b"outside",
        b"sub",
        b"victim",
        b"\xff",
        b"e\xcc\x81",
    ]);
    prop_oneof![
        prop::collection::vec(any::<u8>(), 0..12),
        prop::collection::vec(piece, 0..5).prop_map(|t| t.concat()),
    ]
}

proptest! {
    /// A name passes exactly when it is a single, ordinary path component
    /// with no NUL byte.
    #[test]
    fn entry_names_pass_only_as_single_components(name in entry_name()) {
        let os = OsStr::from_bytes(&name);
        let single = {
            let mut c = Path::new(os).components();
            matches!((c.next(), c.next()), (Some(Component::Normal(n)), None) if n.as_bytes() == name)
        };
        let expected = single && !name.contains(&0) && !name.contains(&b'/');
        prop_assert_eq!(fsops::check_entry_name(os).is_ok(), expected, "{:?}", os);
    }

    /// Descriptor-relative removal with any name only ever affects entries
    /// of the directory it was given.
    #[test]
    fn descriptor_relative_operations_stay_in_their_directory(names in prop::collection::vec(entry_name(), 1..6)) {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let sandbox = base.join("sandbox");
        write(&base.join("outside"), b"keep");
        write(&base.join("sub/victim"), b"keep");
        write(&sandbox.join("victim"), b"x");
        write(&sandbox.join("sub/victim"), b"x");
        fs::create_dir_all(sandbox.join("empty")).unwrap();
        symlink(base.join("outside"), sandbox.join("outside")).unwrap();
        symlink(base.join("sub"), sandbox.join("linkdir")).unwrap();
        let before = snapshot(&base);

        let fd = fsops::open_dir_nofollow(&sandbox).unwrap();
        // Symlinks are never followed, not even to a directory.
        prop_assert!(fsops::open_dir_at(fsops::borrow(&fd), OsStr::new("linkdir")).is_err());
        prop_assert!(fsops::open_dir_nofollow(&sandbox.join("linkdir")).is_err());
        for name in &names {
            let os = OsStr::from_bytes(name);
            let ok = fsops::check_entry_name(os).is_ok();
            // Any operation on a rejected name must fail without touching anything.
            for result in [
                fsops::stat_at(fsops::borrow(&fd), os).map(|_| ()),
                fsops::open_dir_at(fsops::borrow(&fd), os).map(|_| ()),
                fsops::unlink_at(fsops::borrow(&fd), os),
                fsops::rmdir_at(fsops::borrow(&fd), os),
            ] {
                if !ok {
                    prop_assert!(result.is_err(), "{:?}", os);
                }
            }
        }
        drop(fd);

        let after = snapshot(&base);
        for (path, node) in &before {
            if !path.starts_with("sandbox") || path == Path::new("sandbox") {
                prop_assert_eq!(after.get(path), Some(node), "{} changed", path.display());
            }
        }
        // Anything that disappeared was a direct entry of the sandbox.
        for path in before.keys().filter(|p| !after.contains_key(*p)) {
            prop_assert_eq!(path.parent(), Some(Path::new("sandbox")), "{}", path.display());
        }
        prop_assert_eq!(fs::read(base.join("outside")).unwrap(), b"keep");
    }
}

// ---------------------------------------------------------------------------
// Stress: the cache is mutated while the cleaner deletes it
// ---------------------------------------------------------------------------

struct StressFixture {
    _tmp: tempfile::TempDir,
    _outside_tmp: tempfile::TempDir,
    home: PathBuf,
    outside: PathBuf,
}

impl StressFixture {
    fn new() -> StressFixture {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap().join("home");
        // A separate temporary directory stands for "everything outside the
        // fixture root"; the mutator points symlinks and hard links at it.
        let outside_tmp = tempfile::tempdir().unwrap();
        let outside = outside_tmp.path().canonicalize().unwrap();
        for i in 0..8 {
            write(&outside.join(format!("target/d{i}/file")), b"outside");
        }
        write(&outside.join("target/file"), b"outside");
        let cache = home.join(".npm/_cacache");
        for d in 0..8 {
            for f in 0..25 {
                write(
                    &cache.join(format!("content-v2/sha512/d{d}/e{f}/blob")),
                    &[b'c'; 64],
                );
            }
        }
        write(&cache.join("index-v5/00/entry"), b"i");
        write(&home.join("Documents/keep.txt"), b"keep");
        write(&home.join("decoy/_cacache/content-v2/precious"), b"keep");
        StressFixture {
            _tmp: tmp,
            _outside_tmp: outside_tmp,
            home,
            outside,
        }
    }

    fn ctx(&self) -> CleanContext {
        CleanContext {
            home: self.home.clone(),
            uid: platform::current_uid(),
            npm_cache: NpmCacheSource::Explicit(self.home.join(".npm")),
            xcode_running: Some(false),
            package_manager_running: Some(false),
            pip_running: Some(false),
            brew_running: Some(false),
            gradle_running: Some(false),
            pod_running: Some(false),
            derived_data: DerivedDataSetting::Given(PrefValue::Missing),
        }
    }
}

/// Renames, symlink swaps, hard links and new files, as fast as possible,
/// until told to stop. Errors are expected (the cleaner races it) and ignored.
fn mutate(home: PathBuf, outside: PathBuf, stop: Arc<AtomicBool>, seed: usize) {
    let sha = home.join(".npm/_cacache/content-v2/sha512");
    let mut n = seed;
    while !stop.load(Ordering::Relaxed) {
        n = n
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let d = (n >> 33) % 8;
        let dir = sha.join(format!("d{d}"));
        // Write only into real directories, so the mutator rarely writes
        // through a symlink it planted itself.
        let real_dir = fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir());
        let _ = match (n >> 40) % 9 {
            // Move a directory away inside the fixture and put a symlink to
            // the outside in its place.
            0 => fs::rename(&dir, home.join(format!("moved-{n}")))
                .and_then(|_| symlink(outside.join(format!("target/d{d}")), &dir)),
            // Replace a file with a symlink to an outside file.
            1 if real_dir => fs::remove_file(dir.join("e0/blob"))
                .and_then(|_| symlink(outside.join("target/file"), dir.join("e0/blob"))),
            // Hard-link an outside file into the cache.
            2 if real_dir => {
                fs::hard_link(outside.join("target/file"), dir.join(format!("hl-{n}")))
            }
            // New files and directories appear.
            3 if real_dir => fs::create_dir_all(dir.join(format!("new-{n}")))
                .and_then(|_| fs::write(dir.join(format!("new-{n}/f")), b"n")),
            // Swap the whole npm directory for a symlink to a decoy and back.
            4 => fs::rename(home.join(".npm"), home.join(".npm-real"))
                .and_then(|_| symlink(home.join("decoy"), home.join(".npm")))
                .and_then(|_| fs::remove_file(home.join(".npm")))
                .and_then(|_| fs::rename(home.join(".npm-real"), home.join(".npm"))),
            // Swap a subdirectory for a symlink to Documents.
            5 if real_dir => fs::rename(dir.join("e1"), home.join(format!("e1-{n}")))
                .and_then(|_| symlink(home.join("Documents"), dir.join("e1"))),
            // Toggle a directory between itself and a symlink to the outside
            // many times, to hit the window between the cleaner's lstat and
            // its open.
            6 | 7 if real_dir => {
                let e = dir.join(format!("e{}", (n >> 50) % 25));
                let parked = home.join(format!("parked-{n}"));
                for _ in 0..50 {
                    if fs::rename(&e, &parked).is_err() {
                        break;
                    }
                    let _ = symlink(outside.join(format!("target/d{d}")), &e);
                    let _ = fs::remove_file(&e);
                    if fs::rename(&parked, &e).is_err() {
                        break;
                    }
                }
                Ok(())
            }
            _ => fs::write(sha.join(format!("loose-{n}")), b"l"),
        };
    }
}

#[test]
fn concurrent_mutation_never_reaches_outside_the_fixture() {
    let rounds: usize = std::env::var("MACCLEAN_STRESS_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    for round in 0..rounds {
        let f = StressFixture::new();
        let outside_before = snapshot(&f.outside);
        let documents_before = snapshot(&f.home.join("Documents"));
        let decoy_before = snapshot(&f.home.join("decoy"));
        let outside_inode = fs::metadata(f.outside.join("target/file")).unwrap().ino();

        let ctx = f.ctx();
        let stop = Arc::new(AtomicBool::new(false));
        // Even rounds race planning too; odd rounds only execution.
        let start_early = round % 2 == 0;
        let spawn = || {
            let (h, o, s) = (f.home.clone(), f.outside.clone(), stop.clone());
            std::thread::spawn(move || mutate(h, o, s, round))
        };
        let mut mutator = start_early.then(spawn);
        let plan = cleaner::plan(&[CategoryId::NpmCache], &ctx);
        if mutator.is_none() {
            mutator = Some(spawn());
        }
        cleaner::execute(&approve(&plan), &ctx);
        stop.store(true, Ordering::Relaxed);
        mutator.unwrap().join().unwrap();

        // MacClean only deletes. Whatever existed outside the cache must
        // still be there, unchanged. (The mutator may add entries.)
        for (name, root, before) in [
            ("outside", f.outside.clone(), &outside_before),
            ("Documents", f.home.join("Documents"), &documents_before),
            ("decoy", f.home.join("decoy"), &decoy_before),
        ] {
            let after = snapshot(&root);
            for (path, node) in before {
                assert_eq!(
                    after.get(path),
                    Some(node),
                    "round {round}: {name}/{} changed",
                    path.display()
                );
            }
        }
        assert_eq!(
            fs::metadata(f.outside.join("target/file")).unwrap().ino(),
            outside_inode
        );
    }
}
