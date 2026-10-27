//! Cleaner safety tests. Every fixture lives in a temporary directory that
//! stands in for the home directory; no real user data is touched.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use macclean::cleaner::{
    self, CategoryId, CategoryState, CleanContext, CleanOutcome, CleanPlan, DerivedDataSetting,
    NpmCacheSource,
};
use macclean::platform::{self, PrefValue};

const XCODE_HASH: &str = "abcdefghijklmnopqrstuvwxyzab";

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap().join("home");
        fs::create_dir(&home).unwrap();
        let f = Fixture { _tmp: tmp, home };
        // npm cache with recognized layout plus things that must survive.
        f.file(".npm/_cacache/content-v2/sha512/ab/cd/blob1", 5000);
        f.file(".npm/_cacache/content-v2/sha512/ef/01/blob2", 7000);
        f.file(".npm/_cacache/index-v5/12/34/entry", 300);
        fs::create_dir_all(f.home.join(".npm/_cacache/tmp")).unwrap();
        f.file(".npm/_cacache/unexpected.txt", 11);
        f.file(".npm/_logs/debug.log", 22);
        f.file(".npm/_npx/abc/package.json", 33);
        f.file("Documents/important.txt", 44);
        // DerivedData with recognized and unrecognized entries.
        let dd = format!("Library/Developer/Xcode/DerivedData/MyApp-{XCODE_HASH}");
        f.file(&format!("{dd}/Build/Products/Debug/MyApp"), 9000);
        f.file(&format!("{dd}/Index.noindex/DataStore/v5/records"), 1000);
        f.file(&format!("{dd}/info.plist"), 100);
        f.file(
            "Library/Developer/Xcode/DerivedData/ModuleCache.noindex/Foundation.pcm",
            2000,
        );
        f.file("Library/Developer/Xcode/DerivedData/SomethingElse/keep", 55);
        f.file("Library/Developer/Xcode/DerivedData/notes.txt", 66);
        f.file(
            "Library/Developer/Xcode/Archives/2026/App.xcarchive/Info.plist",
            77,
        );
        f
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    fn file(&self, rel: &str, len: usize) {
        let p = self.p(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![b'x'; len]).unwrap();
    }

    fn ctx(&self) -> CleanContext {
        CleanContext {
            home: self.home.clone(),
            uid: platform::current_uid(),
            npm_cache: NpmCacheSource::Explicit(self.p(".npm")),
            xcode_running: Some(false),
            package_manager_running: Some(false),
            derived_data: DerivedDataSetting::Given(PrefValue::Missing),
        }
    }

    fn plan(&self, cats: &[CategoryId]) -> CleanPlan {
        cleaner::plan(cats, &self.ctx())
    }

    fn assert_survivors(&self) {
        for rel in [
            ".npm/_cacache/unexpected.txt",
            ".npm/_logs/debug.log",
            ".npm/_npx/abc/package.json",
            "Documents/important.txt",
            "Library/Developer/Xcode/DerivedData/SomethingElse/keep",
            "Library/Developer/Xcode/DerivedData/notes.txt",
            "Library/Developer/Xcode/Archives/2026/App.xcarchive/Info.plist",
        ] {
            assert!(self.p(rel).exists(), "{rel} must not be deleted");
        }
    }
}

/// Sum of allocated bytes of all regular files below `p` (std, path-based).
fn allocated_under(p: &Path) -> (u64, u64) {
    let mut files = 0;
    let mut bytes = 0;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let m = e.metadata().unwrap();
            if m.is_dir() {
                stack.push(e.path());
            } else if m.is_file() {
                files += 1;
                bytes += m.blocks() * 512;
            }
        }
    }
    (files, bytes)
}

fn npm(outcome: &CleanOutcome) -> &cleaner::CategoryOutcome {
    outcome
        .categories
        .iter()
        .find(|c| c.category == CategoryId::NpmCache)
        .unwrap()
}

#[test]
fn dry_run_plan_deletes_nothing_and_estimates_accurately() {
    let f = Fixture::new();
    let (_, npm_bytes) = allocated_under(&f.p(".npm/_cacache/content-v2"));
    let (_, idx_bytes) = allocated_under(&f.p(".npm/_cacache/index-v5"));
    let plan = f.plan(&CategoryId::ALL);
    assert!(plan.dry_run);

    let npm = &plan.categories[0];
    assert_eq!(npm.state, CategoryState::Ready);
    let names: Vec<_> = npm.candidates.iter().map(|c| c.path.clone()).collect();
    assert_eq!(
        names,
        vec![
            f.p(".npm/_cacache/content-v2"),
            f.p(".npm/_cacache/index-v5"),
            f.p(".npm/_cacache/tmp"),
        ]
    );
    assert_eq!(npm.estimate.files, 3);
    assert_eq!(npm.estimate.logical_bytes, 12_300);
    assert_eq!(npm.estimate.allocated_bytes, npm_bytes + idx_bytes);
    assert_eq!(npm.not_eligible.len(), 1);
    assert_eq!(
        npm.not_eligible[0].path,
        f.p(".npm/_cacache/unexpected.txt")
    );

    let xc = &plan.categories[1];
    assert_eq!(xc.state, CategoryState::Ready);
    assert_eq!(xc.candidates.len(), 2);
    assert_eq!(xc.estimate.files, 4);
    assert_eq!(xc.not_eligible.len(), 2);

    // Nothing was deleted by planning.
    assert!(f.p(".npm/_cacache/content-v2/sha512/ab/cd/blob1").exists());
    assert!(
        f.p(&format!(
            "Library/Developer/Xcode/DerivedData/MyApp-{XCODE_HASH}/info.plist"
        ))
        .exists()
    );
    f.assert_survivors();
}

#[test]
fn execute_deletes_only_verified_candidates_and_reports_exact_bytes() {
    let f = Fixture::new();
    let (c_files, c_bytes) = allocated_under(&f.p(".npm/_cacache/content-v2"));
    let (i_files, i_bytes) = allocated_under(&f.p(".npm/_cacache/index-v5"));
    let dd = f.p(&format!(
        "Library/Developer/Xcode/DerivedData/MyApp-{XCODE_HASH}"
    ));
    let (d_files, d_bytes) = allocated_under(&dd);
    let (m_files, m_bytes) =
        allocated_under(&f.p("Library/Developer/Xcode/DerivedData/ModuleCache.noindex"));

    let plan = f.plan(&CategoryId::ALL);
    let out = cleaner::execute(&plan, &f.ctx());
    assert!(!out.is_partial(), "{out:#?}");

    let n = npm(&out);
    assert_eq!(n.removed.files, c_files + i_files);
    assert_eq!(n.removed.allocated_bytes, c_bytes + i_bytes);
    assert_eq!(n.removed.logical_bytes, 12_300);
    let x = &out.categories[1];
    assert_eq!(x.removed.files, d_files + m_files);
    assert_eq!(x.removed.allocated_bytes, d_bytes + m_bytes);

    assert!(!f.p(".npm/_cacache/content-v2").exists());
    assert!(!f.p(".npm/_cacache/index-v5").exists());
    assert!(!f.p(".npm/_cacache/tmp").exists());
    assert!(!dd.exists());
    assert!(
        !f.p("Library/Developer/Xcode/DerivedData/ModuleCache.noindex")
            .exists()
    );
    // Category roots themselves are kept.
    assert!(f.p(".npm/_cacache").is_dir());
    assert!(f.p("Library/Developer/Xcode/DerivedData").is_dir());
    f.assert_survivors();
}

#[test]
fn selected_category_only() {
    let f = Fixture::new();
    let plan = f.plan(&[CategoryId::NpmCache]);
    assert_eq!(plan.categories.len(), 1);
    cleaner::execute(&plan, &f.ctx());
    assert!(!f.p(".npm/_cacache/content-v2").exists());
    assert!(
        f.p(&format!(
            "Library/Developer/Xcode/DerivedData/MyApp-{XCODE_HASH}/info.plist"
        ))
        .exists()
    );
    f.assert_survivors();
}

#[test]
fn npm_cache_outside_home_is_refused() {
    let f = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let cache = outside.path().canonicalize().unwrap();
    fs::create_dir_all(cache.join("_cacache/content-v2")).unwrap();
    fs::write(cache.join("_cacache/content-v2/blob"), b"data").unwrap();
    let mut ctx = f.ctx();
    ctx.npm_cache = NpmCacheSource::Explicit(cache.clone());
    let plan = cleaner::plan(&[CategoryId::NpmCache], &ctx);
    assert_eq!(plan.categories[0].state, CategoryState::Refused);
    assert!(plan.categories[0].candidates.is_empty());
    let out = cleaner::execute(&plan, &ctx);
    assert!(out.categories.is_empty());
    assert!(cache.join("_cacache/content-v2/blob").exists());
    f.assert_survivors();
}

#[test]
fn npm_cache_in_protected_location_is_refused() {
    let f = Fixture::new();
    f.file("Documents/_cacache/content-v2/blob", 10);
    for dir in ["Documents", "Library/Application Support/x", ".ssh"] {
        f.file(&format!("{dir}/_cacache/content-v2/blob"), 10);
        let mut ctx = f.ctx();
        ctx.npm_cache = NpmCacheSource::Explicit(f.p(dir));
        let plan = cleaner::plan(&[CategoryId::NpmCache], &ctx);
        assert_eq!(plan.categories[0].state, CategoryState::Refused, "{dir}");
        cleaner::execute(&plan, &ctx);
        assert!(f.p(&format!("{dir}/_cacache/content-v2/blob")).exists());
    }
    f.assert_survivors();
}

#[test]
fn npm_cache_path_with_traversal_is_normalized_or_refused() {
    let f = Fixture::new();
    let mut ctx = f.ctx();
    // "../home/Documents" style input resolves to a protected area.
    f.file("Documents/_cacache/content-v2/blob", 10);
    ctx.npm_cache = NpmCacheSource::Explicit(f.p(".npm/../Documents"));
    let plan = cleaner::plan(&[CategoryId::NpmCache], &ctx);
    assert_eq!(plan.categories[0].state, CategoryState::Refused);
    cleaner::execute(&plan, &ctx);
    assert!(f.p("Documents/_cacache/content-v2/blob").exists());
    f.assert_survivors();
}

#[test]
fn unrecognized_npm_layout_is_refused() {
    let f = Fixture::new();
    f.file("weird/_cacache/photos/cat.jpg", 100);
    let mut ctx = f.ctx();
    ctx.npm_cache = NpmCacheSource::Explicit(f.p("weird"));
    let plan = cleaner::plan(&[CategoryId::NpmCache], &ctx);
    assert_eq!(plan.categories[0].state, CategoryState::Refused);
    assert!(plan.categories[0].candidates.is_empty());
    cleaner::execute(&plan, &ctx);
    assert!(f.p("weird/_cacache/photos/cat.jpg").exists());
    f.assert_survivors();
}

#[test]
fn symlink_inside_candidate_is_unlinked_but_target_untouched() {
    let f = Fixture::new();
    symlink(
        f.p("Documents"),
        f.p(".npm/_cacache/content-v2/sha512/docs-link"),
    )
    .unwrap();
    symlink(
        f.p("Documents/important.txt"),
        f.p(".npm/_cacache/index-v5/file-link"),
    )
    .unwrap();
    let plan = f.plan(&[CategoryId::NpmCache]);
    let out = cleaner::execute(&plan, &f.ctx());
    assert!(!out.is_partial(), "{out:#?}");
    assert_eq!(npm(&out).removed.symlinks, 2);
    assert!(!f.p(".npm/_cacache/content-v2").exists());
    assert_eq!(fs::read(f.p("Documents/important.txt")).unwrap().len(), 44);
    f.assert_survivors();
}

#[test]
fn symlinked_candidate_is_forbidden_and_target_untouched() {
    let f = Fixture::new();
    fs::remove_dir_all(f.p(".npm/_cacache/tmp")).unwrap();
    symlink(f.p("Documents"), f.p(".npm/_cacache/tmp")).unwrap();
    let plan = f.plan(&[CategoryId::NpmCache]);
    let cat = &plan.categories[0];
    assert!(
        cat.candidates
            .iter()
            .all(|c| c.path != f.p(".npm/_cacache/tmp"))
    );
    assert!(
        cat.not_eligible
            .iter()
            .any(|r| r.path == f.p(".npm/_cacache/tmp")
                && r.risk == macclean::policy::Risk::Forbidden)
    );
    cleaner::execute(&plan, &f.ctx());
    assert!(f.p(".npm/_cacache/tmp").is_symlink());
    f.assert_survivors();
}

#[test]
fn symlinked_category_roots_are_refused() {
    let f = Fixture::new();
    let elsewhere = f.p("elsewhere-dd");
    fs::create_dir_all(elsewhere.join(format!("Other-{XCODE_HASH}"))).unwrap();
    fs::write(elsewhere.join(format!("Other-{XCODE_HASH}/x")), b"1").unwrap();
    fs::rename(
        f.p("Library/Developer/Xcode/DerivedData"),
        f.p("Library/Developer/Xcode/DerivedData.bak"),
    )
    .unwrap();
    symlink(&elsewhere, f.p("Library/Developer/Xcode/DerivedData")).unwrap();

    fs::rename(f.p(".npm/_cacache"), f.p(".npm/_cacache.real")).unwrap();
    symlink(f.p(".npm/_cacache.real"), f.p(".npm/_cacache")).unwrap();

    let plan = f.plan(&CategoryId::ALL);
    for c in &plan.categories {
        assert_eq!(c.state, CategoryState::Refused, "{:?}", c.category);
    }
    cleaner::execute(&plan, &f.ctx());
    assert!(elsewhere.join(format!("Other-{XCODE_HASH}/x")).exists());
    assert!(
        f.p(".npm/_cacache.real/content-v2/sha512/ab/cd/blob1")
            .exists()
    );
}

#[test]
fn ancestor_swapped_for_symlink_after_planning_is_refused() {
    let f = Fixture::new();
    let plan = f.plan(&[CategoryId::NpmCache]);
    assert_eq!(plan.categories[0].state, CategoryState::Ready);

    // Attacker swaps ~/.npm for a link to a look-alike tree after planning.
    let decoy = f.p("Documents/decoy");
    fs::create_dir_all(decoy.join("_cacache/content-v2")).unwrap();
    fs::write(decoy.join("_cacache/content-v2/precious"), b"keep").unwrap();
    fs::rename(f.p(".npm"), f.p(".npm-moved")).unwrap();
    symlink(&decoy, f.p(".npm")).unwrap();

    let out = cleaner::execute(&plan, &f.ctx());
    assert!(npm(&out).refused.is_some(), "{out:#?}");
    assert_eq!(npm(&out).removed.files, 0);
    assert!(decoy.join("_cacache/content-v2/precious").exists());
    assert!(
        f.p(".npm-moved/_cacache/content-v2/sha512/ab/cd/blob1")
            .exists()
    );
}

#[test]
fn root_replaced_after_planning_is_refused() {
    let f = Fixture::new();
    let plan = f.plan(&[CategoryId::XcodeDerivedData]);
    let dd = f.p("Library/Developer/Xcode/DerivedData");
    fs::rename(&dd, f.p("Library/Developer/Xcode/DerivedData.old")).unwrap();
    fs::create_dir_all(dd.join(format!("MyApp-{XCODE_HASH}"))).unwrap();
    fs::write(dd.join(format!("MyApp-{XCODE_HASH}/new")), b"n").unwrap();
    let out = cleaner::execute(&plan, &f.ctx());
    assert!(out.categories[0].refused.is_some());
    assert!(dd.join(format!("MyApp-{XCODE_HASH}/new")).exists());
    assert!(
        f.p(&format!(
            "Library/Developer/Xcode/DerivedData.old/MyApp-{XCODE_HASH}/info.plist"
        ))
        .exists()
    );
}

#[test]
fn candidate_replaced_after_planning_is_rejected() {
    let f = Fixture::new();
    let plan = f.plan(&[CategoryId::NpmCache]);
    fs::rename(
        f.p(".npm/_cacache/content-v2"),
        f.p(".npm/_cacache/content-v2-moved"),
    )
    .unwrap();
    f.file(".npm/_cacache/content-v2/new-blob", 10);
    let out = cleaner::execute(&plan, &f.ctx());
    let n = npm(&out);
    assert_eq!(n.rejected_candidates.len(), 1);
    assert_eq!(
        n.rejected_candidates[0].path,
        f.p(".npm/_cacache/content-v2")
    );
    assert!(out.is_partial());
    // The replacement and the moved original are untouched; others are removed.
    assert!(f.p(".npm/_cacache/content-v2/new-blob").exists());
    assert!(
        f.p(".npm/_cacache/content-v2-moved/sha512/ab/cd/blob1")
            .exists()
    );
    assert!(!f.p(".npm/_cacache/index-v5").exists());
    f.assert_survivors();
}

#[test]
fn files_changed_or_added_after_planning_are_kept() {
    let f = Fixture::new();
    let plan = f.plan(&[CategoryId::NpmCache]);
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(
        f.p(".npm/_cacache/content-v2/sha512/ab/cd/blob1"),
        b"rewritten",
    )
    .unwrap();
    f.file(".npm/_cacache/index-v5/12/34/added-later", 5);
    let out = cleaner::execute(&plan, &f.ctx());
    let n = npm(&out);
    assert!(out.is_partial());
    assert_eq!(n.removed.skipped_total, 2, "{n:#?}");
    assert!(
        n.removed
            .skipped
            .iter()
            .all(|s| s.reason.contains("modified after"))
    );
    assert_eq!(
        fs::read(f.p(".npm/_cacache/content-v2/sha512/ab/cd/blob1")).unwrap(),
        b"rewritten"
    );
    assert!(f.p(".npm/_cacache/index-v5/12/34/added-later").exists());
    // Unchanged files were removed.
    assert!(!f.p(".npm/_cacache/content-v2/sha512/ef/01/blob2").exists());
    assert!(!f.p(".npm/_cacache/index-v5/12/34/entry").exists());
    f.assert_survivors();
}

#[test]
fn hard_linked_files_are_skipped() {
    let f = Fixture::new();
    fs::hard_link(
        f.p("Documents/important.txt"),
        f.p(".npm/_cacache/index-v5/hardlink"),
    )
    .unwrap();
    let plan = f.plan(&[CategoryId::NpmCache]);
    let out = cleaner::execute(&plan, &f.ctx());
    assert!(out.is_partial());
    assert!(f.p(".npm/_cacache/index-v5/hardlink").exists());
    assert_eq!(fs::read(f.p("Documents/important.txt")).unwrap().len(), 44);
    assert!(!f.p(".npm/_cacache/content-v2").exists());
    f.assert_survivors();
}

#[test]
fn partial_failure_is_reported_and_remaining_items_are_removed() {
    let f = Fixture::new();
    let locked = f.p(".npm/_cacache/content-v2/sha512/ab/cd");
    let plan = f.plan(&[CategoryId::NpmCache]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    let out = cleaner::execute(&plan, &f.ctx());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let n = npm(&out);
    assert!(out.is_partial());
    assert!(n.removed.errors_total >= 1, "{n:#?}");
    assert!(locked.join("blob1").exists());
    assert!(!f.p(".npm/_cacache/content-v2/sha512/ef").exists());
    assert!(!f.p(".npm/_cacache/index-v5").exists());
    // Only the files actually removed are counted.
    assert_eq!(n.removed.files, 2);
    assert_eq!(n.removed.logical_bytes, 7_300);
    f.assert_survivors();
}

#[test]
fn tampered_candidate_names_are_rejected() {
    let f = Fixture::new();
    let mut plan = f.plan(&[CategoryId::NpmCache]);
    let cand = &mut plan.categories[0].candidates[0];
    cand.name = "../../Documents".into();
    cand.path = f.p(".npm/_cacache/../../Documents");
    let out = cleaner::execute(&plan, &f.ctx());
    assert_eq!(npm(&out).rejected_candidates.len(), 1);
    assert!(f.p("Documents/important.txt").exists());
    f.assert_survivors();
}

#[test]
fn xcode_running_refuses_derived_data() {
    let f = Fixture::new();
    let mut ctx = f.ctx();
    ctx.xcode_running = Some(true);
    let plan = cleaner::plan(&[CategoryId::XcodeDerivedData], &ctx);
    assert_eq!(plan.categories[0].state, CategoryState::Refused);
    // Xcode starting between plan and execution is also caught.
    let plan = f.plan(&[CategoryId::XcodeDerivedData]);
    let out = cleaner::execute(&plan, &ctx);
    assert!(out.categories[0].refused.is_some());
    assert!(
        f.p(&format!(
            "Library/Developer/Xcode/DerivedData/MyApp-{XCODE_HASH}/info.plist"
        ))
        .exists()
    );
}

#[test]
fn package_manager_running_refuses_npm_cache() {
    let f = Fixture::new();
    let mut ctx = f.ctx();
    ctx.package_manager_running = Some(true);
    let plan = cleaner::plan(&CategoryId::ALL, &ctx);
    assert_eq!(plan.categories[0].state, CategoryState::Refused);
    assert!(plan.categories[0].candidates.is_empty());
    assert!(
        plan.categories[0]
            .state_reason
            .as_deref()
            .is_some_and(|r| r.contains("npm"))
    );
    // Other categories are unaffected.
    assert_eq!(plan.categories[1].state, CategoryState::Ready);

    // npm starting between plan and execution is also caught.
    let plan = f.plan(&[CategoryId::NpmCache]);
    assert_eq!(plan.categories[0].state, CategoryState::Ready);
    let out = cleaner::execute(&plan, &ctx);
    assert!(npm(&out).refused.is_some(), "{out:#?}");
    assert_eq!(npm(&out).removed.files, 0);
    assert!(out.is_partial());
    assert!(f.p(".npm/_cacache/content-v2/sha512/ab/cd/blob1").exists());
    assert!(f.p(".npm/_cacache/index-v5/12/34/entry").exists());
    f.assert_survivors();
}

#[test]
fn root_owned_by_another_user_is_refused() {
    let f = Fixture::new();
    let mut ctx = f.ctx();
    ctx.uid = ctx.uid.wrapping_add(4242);
    let plan = cleaner::plan(&CategoryId::ALL, &ctx);
    for c in &plan.categories {
        assert_eq!(c.state, CategoryState::Refused);
    }
    f.assert_survivors();
}

#[test]
fn missing_categories_are_reported_not_present() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let ctx = CleanContext {
        home: home.clone(),
        uid: platform::current_uid(),
        npm_cache: NpmCacheSource::Explicit(home.join(".npm")),
        xcode_running: Some(false),
        package_manager_running: Some(false),
        derived_data: DerivedDataSetting::Given(PrefValue::Missing),
    };
    let plan = cleaner::plan(&CategoryId::ALL, &ctx);
    for c in &plan.categories {
        assert_eq!(c.state, CategoryState::NotPresent);
    }
    assert!(!plan.has_candidates());
    assert!(cleaner::execute(&plan, &ctx).categories.is_empty());
}

fn with_derived_data(f: &Fixture, value: &str) -> CleanContext {
    let mut ctx = f.ctx();
    ctx.derived_data = DerivedDataSetting::Given(PrefValue::String(value.to_string()));
    ctx
}

#[test]
fn custom_derived_data_location_is_cleaned_like_the_default() {
    let f = Fixture::new();
    let custom = format!("Builds/DerivedData/Other-{XCODE_HASH}");
    f.file(&format!("{custom}/Build/x"), 500);
    f.file("Builds/DerivedData/ModuleCache.noindex/m.pcm", 10);
    f.file("Builds/DerivedData/readme.txt", 10);
    let ctx = with_derived_data(&f, &f.p("Builds/DerivedData").display().to_string());

    let plan = cleaner::plan(&[CategoryId::XcodeDerivedData], &ctx);
    let c = &plan.categories[0];
    assert_eq!(c.state, CategoryState::Ready, "{c:#?}");
    assert_eq!(c.root.as_deref(), Some(f.p("Builds/DerivedData").as_path()));
    assert!(c.root_source.as_deref().unwrap().contains("custom"));
    assert_eq!(c.candidates.len(), 2);

    let out = cleaner::execute(&plan, &ctx);
    assert!(!out.is_partial(), "{out:#?}");
    assert!(!f.p(&custom).exists());
    assert!(!f.p("Builds/DerivedData/ModuleCache.noindex").exists());
    assert!(f.p("Builds/DerivedData/readme.txt").exists());
    // The default location is not touched when a custom one is set.
    assert!(
        f.p(&format!(
            "Library/Developer/Xcode/DerivedData/MyApp-{XCODE_HASH}/info.plist"
        ))
        .exists()
    );
    f.assert_survivors();
}

#[test]
fn unsafe_custom_derived_data_locations_are_refused() {
    let f = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let outside_dd = outside.path().canonicalize().unwrap().join("DD");
    let project = format!("Proj-{XCODE_HASH}/keep");
    for dir in [
        f.p("Documents/DD"),
        f.p("Library/Application Support/DD"),
        f.p(".ssh/DD"),
        outside_dd.clone(),
    ] {
        fs::create_dir_all(dir.join(format!("Proj-{XCODE_HASH}"))).unwrap();
        fs::write(dir.join(&project), b"k").unwrap();
    }
    fs::create_dir_all(f.p("real-dd")).unwrap();
    symlink(f.p("Documents/DD"), f.p("linked-dd")).unwrap();

    for (value, expect) in [
        (
            f.p("Documents/DD").display().to_string(),
            CategoryState::Refused,
        ),
        (
            f.p("Library/Application Support/DD").display().to_string(),
            CategoryState::Refused,
        ),
        (f.p(".ssh/DD").display().to_string(), CategoryState::Refused),
        (f.home.display().to_string(), CategoryState::Refused),
        (outside_dd.display().to_string(), CategoryState::Refused),
        (
            f.p("linked-dd").display().to_string(),
            CategoryState::Refused,
        ),
        (
            f.p("real-dd/../Documents/DD").display().to_string(),
            CategoryState::Refused,
        ),
        ("DerivedData".to_string(), CategoryState::Refused),
        (
            f.p("missing-dd").display().to_string(),
            CategoryState::NotPresent,
        ),
    ] {
        let ctx = with_derived_data(&f, &value);
        let plan = cleaner::plan(&[CategoryId::XcodeDerivedData], &ctx);
        let c = &plan.categories[0];
        assert_eq!(c.state, expect, "{value}: {c:#?}");
        assert!(c.candidates.is_empty(), "{value}");
        if expect == CategoryState::Refused {
            assert!(c.state_reason.is_some(), "{value}");
        }
        cleaner::execute(&plan, &ctx);
    }
    for dir in [
        f.p("Documents/DD"),
        f.p("Library/Application Support/DD"),
        f.p(".ssh/DD"),
        outside_dd,
    ] {
        assert!(dir.join(&project).exists(), "{}", dir.display());
    }
    f.assert_survivors();
}
