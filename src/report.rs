//! Text and JSON rendering of MacClean results.

use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::cleaner::{CategoryState, CleanOutcome, CleanPlan, Tally};
use crate::monitor::{self, MemoryMetrics, Sample};
use crate::platform::{self, ArchInfo, OsInfo, VolumeStats};
use crate::scanner::{ScanReport, SizeMetric, SkipReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}

/// Formats bytes with decimal units (1 GB = 10^9 bytes), as Finder does.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    format!("{v:.1} {}", UNITS[i])
}

pub fn human_signed_bytes(n: i128) -> String {
    let mag = human_bytes(u64::try_from(n.unsigned_abs()).unwrap_or(u64::MAX));
    if n < 0 {
        format!("-{mag}")
    } else {
        format!("+{mag}")
    }
}

pub fn human_duration(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    match (d, h) {
        (0, 0) => format!("{m}m {}s", secs % 60),
        (0, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h {m}m"),
    }
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        "n/a".into()
    } else {
        format!("{:.1}%", part as f64 * 100.0 / whole as f64)
    }
}

pub fn write_json<T: Serialize>(w: &mut dyn Write, value: &T) -> crate::errors::Result<()> {
    serde_json::to_writer_pretty(&mut *w, value)?;
    writeln!(w)?;
    Ok(())
}

pub fn write_json_line<T: Serialize>(w: &mut dyn Write, value: &T) -> crate::errors::Result<()> {
    serde_json::to_writer(&mut *w, value)?;
    writeln!(w)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub macclean_version: &'static str,
    pub os: OsInfo,
    pub arch: ArchInfo,
    pub logical_cores: Option<usize>,
    pub memory: MemoryMetrics,
    pub disk: Option<VolumeStats>,
    pub uptime_secs: Option<u64>,
    pub notes: Vec<String>,
}

pub fn collect_status() -> StatusReport {
    let mut notes = Vec::new();
    let disk = match platform::home_dir() {
        Some(h) => match platform::volume_stats(&h) {
            Ok(v) => Some(v),
            Err(e) => {
                notes.push(format!("Disk statistics unavailable: {e}"));
                None
            }
        },
        None => {
            notes.push("Disk statistics unavailable: home directory unknown".into());
            None
        }
    };
    let uptime = sysinfo::System::uptime();
    StatusReport {
        macclean_version: env!("CARGO_PKG_VERSION"),
        os: platform::os_info(),
        arch: platform::arch_info(),
        logical_cores: std::thread::available_parallelism().ok().map(|n| n.get()),
        memory: monitor::memory_now(),
        disk,
        uptime_secs: (uptime > 0).then_some(uptime),
        notes,
    }
}

fn opt<T: std::fmt::Display>(v: &Option<T>) -> String {
    v.as_ref()
        .map(|x| x.to_string())
        .unwrap_or_else(|| "unavailable".into())
}

fn render_disk(w: &mut dyn Write, d: &VolumeStats) -> io::Result<()> {
    writeln!(
        w,
        "  Disk ({} at {}): {} total, {} used ({}), {} available",
        d.filesystem,
        d.mount_point.display(),
        human_bytes(d.total_bytes),
        human_bytes(d.used_bytes),
        percent(d.used_bytes, d.total_bytes),
        human_bytes(d.available_bytes)
    )
}

fn render_memory(w: &mut dyn Write, m: &MemoryMetrics) -> io::Result<()> {
    writeln!(
        w,
        "  Memory: {} total, {} used ({}), {} available; swap {} of {} used",
        human_bytes(m.total_bytes),
        human_bytes(m.used_bytes),
        percent(m.used_bytes, m.total_bytes),
        human_bytes(m.available_bytes),
        human_bytes(m.swap_used_bytes),
        human_bytes(m.swap_total_bytes)
    )
}

pub fn render_status(w: &mut dyn Write, s: &StatusReport) -> io::Result<()> {
    writeln!(w, "MacClean {} — system status", s.macclean_version)?;
    writeln!(
        w,
        "  macOS: {} (build {}, Darwin {})",
        opt(&s.os.product_version),
        opt(&s.os.build_version),
        opt(&s.os.kernel_release)
    )?;
    writeln!(
        w,
        "  Hardware: {} — {}, {} logical cores",
        s.arch.hardware_arch,
        opt(&s.arch.cpu_brand),
        opt(&s.logical_cores)
    )?;
    let rosetta = match s.arch.rosetta_translated {
        Some(true) => " (running under Rosetta 2)",
        _ => "",
    };
    writeln!(w, "  MacClean binary: {}{rosetta}", s.arch.binary_arch)?;
    render_memory(w, &s.memory)?;
    match &s.disk {
        Some(d) => render_disk(w, d)?,
        None => writeln!(w, "  Disk: unavailable")?,
    }
    writeln!(
        w,
        "  Uptime: {}",
        s.uptime_secs
            .map(human_duration)
            .unwrap_or_else(|| "unavailable".into())
    )?;
    writeln!(
        w,
        "  Note: available disk space excludes APFS purgeable space, which Finder may include."
    )?;
    for n in &s.notes {
        writeln!(w, "  Note: {n}")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// analyze
// ---------------------------------------------------------------------------

fn skip_reason(r: &SkipReason) -> String {
    match r {
        SkipReason::PermissionDenied => "permission denied".into(),
        SkipReason::Vanished => "disappeared or does not exist".into(),
        SkipReason::SymlinkRootNotFollowed => {
            "is a symbolic link (not followed; pass the target path instead)".into()
        }
        SkipReason::Cycle => "filesystem cycle".into(),
        SkipReason::Error(e) => e.clone(),
    }
}

pub fn render_scan(w: &mut dyn Write, r: &ScanReport) -> io::Result<()> {
    let roots: Vec<String> = r.roots.iter().map(|p| p.display().to_string()).collect();
    writeln!(w, "Storage analysis of {}", roots.join(", "))?;
    let t = &r.totals;
    writeln!(
        w,
        "  {} files in {} directories: {} logical size, {} allocated on disk",
        t.files,
        t.directories,
        human_bytes(t.logical_bytes),
        human_bytes(t.allocated_bytes)
    )?;
    writeln!(
        w,
        "  {} symbolic links not followed, {} other entries, {} extra hard links counted once",
        t.symlinks_not_followed, t.other_entries, t.hardlinks_deduplicated
    )?;
    writeln!(
        w,
        "  Scan time: {:.2}s. Ranked by {} size.",
        r.elapsed_ms as f64 / 1000.0,
        match r.sort_by {
            SizeMetric::Allocated => "allocated",
            SizeMetric::Logical => "logical",
        }
    )?;
    let size = |logical: u64, allocated: u64| match r.sort_by {
        SizeMetric::Allocated => format!(
            "{:>10}  (logical {})",
            human_bytes(allocated),
            human_bytes(logical)
        ),
        SizeMetric::Logical => format!(
            "{:>10}  (allocated {})",
            human_bytes(logical),
            human_bytes(allocated)
        ),
    };
    writeln!(w, "\nLargest directories (sizes include subdirectories):")?;
    if r.top_directories.is_empty() {
        writeln!(w, "  (none)")?;
    }
    for d in &r.top_directories {
        writeln!(
            w,
            "  {}  {:>8} files  {}",
            size(d.logical_bytes, d.allocated_bytes),
            d.file_count.unwrap_or(0),
            d.path.display()
        )?;
    }
    writeln!(w, "\nLargest files:")?;
    if r.top_files.is_empty() {
        writeln!(w, "  (none)")?;
    }
    for f in &r.top_files {
        writeln!(
            w,
            "  {}  {}",
            size(f.logical_bytes, f.allocated_bytes),
            f.path.display()
        )?;
    }
    if !r.mounts_not_crossed.is_empty() {
        writeln!(
            w,
            "\nNot descended into {} mount point(s) of other filesystems (use --cross-mounts):",
            r.mounts_not_crossed.len()
        )?;
        for m in r.mounts_not_crossed.iter().take(20) {
            writeln!(w, "  {}", m.display())?;
        }
    }
    if r.incomplete {
        writeln!(
            w,
            "\nINCOMPLETE SCAN: {} path(s) could not be read; totals exclude them.",
            r.skipped_total
        )?;
        for s in r.skipped.iter().take(25) {
            writeln!(w, "  {} — {}", s.path.display(), skip_reason(&s.reason))?;
        }
        if r.skipped_total > 25 {
            writeln!(
                w,
                "  … {} more (see --format json for up to {})",
                r.skipped_total - 25,
                crate::scanner::MAX_RECORDED_SKIPS
            )?;
        }
    }
    writeln!(
        w,
        "\nNote: allocated size is what files occupy now. Deleting them may reclaim less \
         (APFS clones, hard links, snapshots, files still open)."
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// clean
// ---------------------------------------------------------------------------

fn tally_line(t: &Tally) -> String {
    format!(
        "{} files, {} directories, {} symlinks — {} allocated ({} logical)",
        t.files,
        t.directories,
        t.symlinks,
        human_bytes(t.allocated_bytes),
        human_bytes(t.logical_bytes)
    )
}

/// Quotes `s` for a POSIX shell command line.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// One explanation for every entry owned by another user, instead of a
/// line per path. MacClean itself never runs sudo; the command is for the
/// user to run, if they want to.
fn render_foreign_owned(
    w: &mut dyn Write,
    count: u64,
    root: Option<&std::path::Path>,
    indent: &str,
) -> io::Result<()> {
    if count == 0 {
        return Ok(());
    }
    writeln!(
        w,
        "{indent}{count} item(s) are owned by another user and are left in place. \
         This usually comes from running the tool with sudo."
    )?;
    if let Some(root) = root {
        writeln!(
            w,
            "{indent}MacClean never uses sudo. To make them yours, run this yourself, then run \
             MacClean again:\n{indent}  sudo chown -R \"$(id -un)\" {}",
            shell_quote(&root.to_string_lossy())
        )?;
    }
    Ok(())
}

fn is_foreign(reason: &str) -> bool {
    reason == crate::cleaner::OWNED_BY_ANOTHER_USER
}

pub fn render_plan(w: &mut dyn Write, p: &CleanPlan) -> io::Result<()> {
    writeln!(
        w,
        "Cleanup plan ({})",
        if p.dry_run {
            "dry run — nothing will be deleted"
        } else {
            "awaiting confirmation"
        }
    )?;
    let mut total = Tally::default();
    for c in &p.categories {
        writeln!(w, "\n[{}] {}", c.category.name(), c.description)?;
        if let Some(root) = &c.root {
            writeln!(
                w,
                "  Root: {}{}",
                root.display(),
                c.root_source
                    .as_ref()
                    .map(|s| format!("  (from {s})"))
                    .unwrap_or_default()
            )?;
        }
        for note in &c.notes {
            writeln!(w, "  Note: {note}")?;
        }
        match c.state {
            CategoryState::NotPresent => {
                writeln!(w, "  Not present: {}", opt(&c.state_reason))?;
            }
            CategoryState::Refused => {
                writeln!(w, "  REFUSED: {}", opt(&c.state_reason))?;
            }
            CategoryState::Empty => writeln!(w, "  Nothing eligible to clean.")?,
            CategoryState::Ready => {}
        }
        for cand in &c.candidates {
            writeln!(w, "  candidate: {}  — {}", cand.path.display(), cand.reason)?;
            writeln!(w, "             {}", tally_line(&cand.estimate))?;
        }
        let mut foreign = 0;
        for r in &c.not_eligible {
            if is_foreign(&r.reason) {
                foreign += 1;
                continue;
            }
            let label = match r.risk {
                crate::policy::Risk::Forbidden => "forbidden",
                _ => "review",
            };
            writeln!(w, "  {label}: {} — {}", r.path.display(), r.reason)?;
        }
        if !c.candidates.is_empty() {
            writeln!(w, "  Estimated: {}", tally_line(&c.estimate))?;
            foreign += c.estimate.foreign_owned;
            let other_skips = c.estimate.skipped_total - c.estimate.foreign_owned;
            if other_skips + c.estimate.errors_total > 0 {
                writeln!(
                    w,
                    "  {} item(s) inside candidates will be skipped, {} could not be read:",
                    other_skips, c.estimate.errors_total
                )?;
                let listed = c.estimate.skipped.iter().filter(|s| !is_foreign(&s.reason));
                for s in listed.chain(&c.estimate.errors).take(10) {
                    writeln!(w, "    {} — {}", s.path.display(), s.reason)?;
                }
            }
        }
        render_foreign_owned(w, foreign, c.root.as_deref(), "  ")?;
        total.absorb(&c.estimate);
    }
    writeln!(w, "\nTotal estimated: {}", tally_line(&total))?;
    writeln!(
        w,
        "Estimates are upper bounds on reclaimable space; APFS clones and snapshots can reduce it."
    )?;
    Ok(())
}

pub fn render_outcome(w: &mut dyn Write, o: &CleanOutcome) -> io::Result<()> {
    writeln!(w, "Cleanup result")?;
    for c in &o.categories {
        writeln!(w, "\n[{}]", c.category.name())?;
        if let Some(why) = &c.refused {
            writeln!(w, "  REFUSED at execution, nothing deleted: {why}")?;
            continue;
        }
        writeln!(w, "  Removed: {}", tally_line(&c.removed))?;
        if let Some(d) = c.observed_available_delta_bytes {
            writeln!(
                w,
                "  Observed change in available volume space: {} (also affected by other activity)",
                human_signed_bytes(d)
            )?;
        }
        for r in &c.rejected_candidates {
            writeln!(w, "  not touched: {} — {}", r.path.display(), r.reason)?;
        }
        let other_skips = c.removed.skipped_total - c.removed.foreign_owned;
        if other_skips > 0 {
            writeln!(w, "  Skipped {other_skips} item(s):")?;
            let listed = c.removed.skipped.iter().filter(|s| !is_foreign(&s.reason));
            for s in listed.take(20) {
                writeln!(w, "    {} — {}", s.path.display(), s.reason)?;
            }
        }
        render_foreign_owned(w, c.removed.foreign_owned, c.root.as_deref(), "  ")?;
        if c.removed.errors_total > 0 {
            writeln!(w, "  Failed to remove {} item(s):", c.removed.errors_total)?;
            for e in c.removed.errors.iter().take(20) {
                writeln!(w, "    {} — {}", e.path.display(), e.reason)?;
            }
        }
    }
    if o.is_partial() {
        writeln!(
            w,
            "\nCleanup was PARTIAL: some planned items were not removed (see above)."
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// monitor
// ---------------------------------------------------------------------------

pub fn render_sample(w: &mut dyn Write, s: &Sample) -> io::Result<()> {
    writeln!(
        w,
        "MacClean monitor — uptime {}, {} processes",
        human_duration(s.uptime_secs),
        s.process_count
    )?;
    writeln!(
        w,
        "  CPU: {:.1}% across {} logical cores{}; load average {:.2} {:.2} {:.2}",
        s.cpu.usage_percent,
        s.cpu.logical_cores,
        s.cpu
            .physical_cores
            .map(|p| format!(" ({p} physical)"))
            .unwrap_or_default(),
        s.cpu.load_average[0],
        s.cpu.load_average[1],
        s.cpu.load_average[2]
    )?;
    render_memory(w, &s.memory)?;
    match &s.disk {
        Some(d) => render_disk(w, d)?,
        None => writeln!(w, "  Disk: unavailable")?,
    }
    writeln!(
        w,
        "\n  {:>7}  {:>7}  {:>10}  {:>10}  NAME   (sorted by {})",
        "PID",
        "CPU%",
        "RSS",
        "FOOTPRINT",
        match s.sorted_by {
            monitor::ProcessSort::Cpu => "CPU",
            monitor::ProcessSort::Memory => "RSS",
            monitor::ProcessSort::Footprint => "footprint",
        }
    )?;
    for p in &s.top_processes {
        writeln!(
            w,
            "  {:>7}  {:>7.1}  {:>10}  {:>10}  {}",
            p.pid,
            p.cpu_percent,
            human_bytes(p.resident_bytes),
            p.footprint_bytes
                .map_or_else(|| "–".to_string(), human_bytes),
            p.name
        )?;
    }
    writeln!(
        w,
        "  CPU% is per core (can exceed 100%). RSS counts shared memory in each process; \
         FOOTPRINT matches Activity Monitor's \"Memory\" column (– = another user's process)."
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct FullReport {
    pub generated_unix_secs: u64,
    pub status: StatusReport,
    pub cleanup_plan: CleanPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analysis: Option<ScanReport>,
}

impl FullReport {
    pub fn new(
        status: StatusReport,
        cleanup_plan: CleanPlan,
        analysis: Option<ScanReport>,
    ) -> Self {
        FullReport {
            generated_unix_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            status,
            cleanup_plan,
            analysis,
        }
    }
}

pub fn render_full(w: &mut dyn Write, r: &FullReport) -> io::Result<()> {
    render_status(w, &r.status)?;
    writeln!(w)?;
    render_plan(w, &r.cleanup_plan)?;
    if let Some(a) = &r.analysis {
        writeln!(w)?;
        render_scan(w, a)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_formatting() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1000), "1.0 kB");
        assert_eq!(human_bytes(1_500_000), "1.5 MB");
        assert_eq!(human_bytes(2_000_000_000_000), "2.0 TB");
        assert_eq!(human_signed_bytes(-1500), "-1.5 kB");
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(human_duration(59), "0m 59s");
        assert_eq!(human_duration(3_700), "1h 1m");
        assert_eq!(human_duration(90_061), "1d 1h 1m");
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(shell_quote("/Users/ann/.npm"), "'/Users/ann/.npm'");
        assert_eq!(shell_quote("/a b/it's"), r"'/a b/it'\''s'");
    }

    #[test]
    fn foreign_owned_entries_are_summarized_not_listed() {
        use crate::cleaner::{
            CategoryId, CategoryOutcome, CategoryPlan, ItemRecord, OWNED_BY_ANOTHER_USER,
            ReviewItem,
        };
        use std::path::PathBuf;

        let root = PathBuf::from("/Users/ann/.npm/_cacache");
        let mut estimate = Tally::default();
        for i in 0..22 {
            estimate.skipped_total += 1;
            estimate.foreign_owned += 1;
            estimate.skipped.push(ItemRecord {
                path: root.join(format!("content-v2/sha512/{i:02}")),
                reason: OWNED_BY_ANOTHER_USER.into(),
            });
        }
        estimate.skipped_total += 1;
        estimate.skipped.push(ItemRecord {
            path: root.join("index-v5/hardlinked"),
            reason: "has other hard links; space would not be reclaimed".into(),
        });
        let plan = CleanPlan {
            dry_run: true,
            created_unix_secs: 0,
            created_ns: 0,
            categories: vec![CategoryPlan {
                category: CategoryId::NpmCache,
                description: CategoryId::NpmCache.description(),
                state: CategoryState::Ready,
                state_reason: None,
                root: Some(root.clone()),
                root_source: None,
                root_id: None,
                candidates: vec![crate::cleaner::Candidate {
                    name: "content-v2".into(),
                    path: root.join("content-v2"),
                    kind: crate::platform::fsops::FileKind::Dir,
                    id: crate::platform::fsops::FileId { dev: 1, ino: 2 },
                    reason: "npm cacache".into(),
                    estimate: estimate.clone(),
                }],
                not_eligible: vec![ReviewItem {
                    path: root.join("tmp"),
                    risk: crate::policy::Risk::Forbidden,
                    reason: OWNED_BY_ANOTHER_USER.into(),
                }],
                notes: Vec::new(),
                estimate: estimate.clone(),
            }],
        };
        let mut out = Vec::new();
        render_plan(&mut out, &plan).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("sha512/00"), "{text}");
        assert!(!text.contains("forbidden:"), "{text}");
        assert!(
            text.contains("23 item(s) are owned by another user"),
            "{text}"
        );
        assert!(text.contains("index-v5/hardlinked"), "{text}");
        assert!(
            text.contains("1 item(s) inside candidates will be skipped"),
            "{text}"
        );
        assert!(
            text.contains(r#"sudo chown -R "$(id -un)" '/Users/ann/.npm/_cacache'"#),
            "{text}"
        );
        assert_eq!(text.matches("owned by another user").count(), 1, "{text}");

        let outcome = CleanOutcome {
            categories: vec![CategoryOutcome {
                category: CategoryId::NpmCache,
                root: Some(root),
                refused: None,
                rejected_candidates: Vec::new(),
                removed: estimate,
                observed_available_delta_bytes: None,
            }],
        };
        let mut out = Vec::new();
        render_outcome(&mut out, &outcome).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Skipped 1 item(s):"), "{text}");
        assert!(
            text.contains("22 item(s) are owned by another user"),
            "{text}"
        );
        assert!(!text.contains("sha512/00"), "{text}");
    }
}
