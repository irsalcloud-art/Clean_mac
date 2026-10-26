//! Read-only system metrics. MacClean never terminates processes or
//! changes system settings.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sysinfo::{
    CpuRefreshKind, MemoryRefreshKind, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System,
};

use crate::platform::{self, VolumeStats};

pub const MIN_INTERVAL_SECS: u64 = 1;
pub const MAX_INTERVAL_SECS: u64 = 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessSort {
    Cpu,
    Memory,
}

#[derive(Debug, Clone, Serialize)]
pub struct CpuMetrics {
    /// Average utilization across all logical cores over the sample interval (0–100).
    pub usage_percent: f32,
    pub logical_cores: usize,
    pub physical_cores: Option<usize>,
    pub load_average: [f64; 3],
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryMetrics {
    pub total_bytes: u64,
    /// App memory + wired + compressed, similar to Activity Monitor's "Memory Used".
    pub used_bytes: u64,
    /// Active + inactive + free + speculative pages (XNU's "available" estimate).
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessMetrics {
    pub pid: u32,
    pub name: String,
    /// Percent of one core; can exceed 100 for multi-threaded processes.
    pub cpu_percent: f32,
    /// Resident set size of this process. Shared pages are counted in every
    /// process that maps them, so these values do not sum to system usage.
    pub resident_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Sample {
    pub timestamp_unix_secs: u64,
    pub interval_secs: u64,
    pub uptime_secs: u64,
    pub cpu: CpuMetrics,
    pub memory: MemoryMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk: Option<VolumeStats>,
    pub process_count: usize,
    pub sorted_by: ProcessSort,
    pub top_processes: Vec<ProcessMetrics>,
    /// Processes MacClean was not permitted to inspect report zero usage.
    pub notes: Vec<String>,
}

pub struct Monitor {
    sys: System,
    top: usize,
    sort: ProcessSort,
    interval: Duration,
}

fn process_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing().with_cpu().with_memory()
}

impl Monitor {
    pub fn new(interval: Duration, top: usize, sort: ProcessSort) -> Monitor {
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::everything()),
        );
        // Prime CPU counters; usage is computed between two refreshes.
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, process_kind());
        Monitor {
            sys,
            top,
            sort,
            interval: interval.max(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL),
        }
    }

    /// Waits one interval, then returns a sample covering it.
    pub fn next_sample(&mut self) -> Sample {
        std::thread::sleep(self.interval);
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::All, true, process_kind());
        self.snapshot()
    }

    fn snapshot(&self) -> Sample {
        let mut procs: Vec<ProcessMetrics> = self
            .sys
            .processes()
            .values()
            .map(|p| ProcessMetrics {
                pid: p.pid().as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                cpu_percent: p.cpu_usage(),
                resident_bytes: p.memory(),
            })
            .collect();
        let process_count = procs.len();
        match self.sort {
            ProcessSort::Cpu => procs.sort_by(|a, b| {
                b.cpu_percent
                    .total_cmp(&a.cpu_percent)
                    .then(b.resident_bytes.cmp(&a.resident_bytes))
            }),
            ProcessSort::Memory => procs.sort_by(|a, b| {
                b.resident_bytes
                    .cmp(&a.resident_bytes)
                    .then(b.cpu_percent.total_cmp(&a.cpu_percent))
            }),
        }
        procs.truncate(self.top);

        let load = System::load_average();
        let mut notes = vec![
            "Process memory is resident set size (RSS); it is not comparable to system memory used."
                .to_string(),
        ];
        let disk = platform::home_dir().and_then(|h| platform::volume_stats(&h).ok());
        if disk.is_none() {
            notes.push("Disk usage is unavailable.".into());
        }
        Sample {
            timestamp_unix_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            interval_secs: self.interval.as_secs(),
            uptime_secs: System::uptime(),
            cpu: CpuMetrics {
                usage_percent: self.sys.global_cpu_usage(),
                logical_cores: self.sys.cpus().len(),
                physical_cores: System::physical_core_count(),
                load_average: [load.one, load.five, load.fifteen],
            },
            memory: memory_metrics(&self.sys),
            disk,
            process_count,
            sorted_by: self.sort,
            top_processes: procs,
            notes,
        }
    }
}

/// Builds memory metrics from a refreshed `System`.
///
/// sysinfo converts VM page counts to bytes with the process page size. An
/// x86_64 process under Rosetta 2 sees 4 KiB pages while the kernel counts
/// 16 KiB pages, which made used/available memory 4x too small. Total memory,
/// swap and process RSS are reported in bytes and need no correction.
fn memory_metrics(sys: &System) -> MemoryMetrics {
    let scale = platform::vm_page_scale();
    let total = sys.total_memory();
    let fix = |v: u64| v.saturating_mul(scale).min(total);
    MemoryMetrics {
        total_bytes: total,
        used_bytes: fix(sys.used_memory()),
        available_bytes: fix(sys.available_memory()),
        swap_total_bytes: sys.total_swap(),
        swap_used_bytes: sys.used_swap(),
    }
}

/// Memory statistics without waiting for a CPU interval (used by `status`).
pub fn memory_now() -> MemoryMetrics {
    let mut sys = System::new();
    sys.refresh_memory();
    memory_metrics(&sys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_values_are_plausible_and_sorted() {
        let mut m = Monitor::new(Duration::from_millis(250), 5, ProcessSort::Memory);
        let s = m.next_sample();
        assert!(s.cpu.logical_cores > 0);
        assert!((0.0..=100.0).contains(&s.cpu.usage_percent));
        assert!(s.memory.total_bytes > 0);
        assert!(s.memory.used_bytes <= s.memory.total_bytes);
        assert!(s.memory.available_bytes <= s.memory.total_bytes);
        assert!(s.uptime_secs > 0);
        assert!(s.process_count > 0);
        assert!(s.top_processes.len() <= 5);
        assert!(
            s.top_processes
                .windows(2)
                .all(|w| w[0].resident_bytes >= w[1].resident_bytes)
        );
        // This test process itself must be visible with a nonzero RSS.
        let me = std::process::id();
        let mut all = Monitor::new(Duration::from_millis(250), usize::MAX, ProcessSort::Cpu);
        let s2 = all.next_sample();
        assert!(
            s2.top_processes
                .iter()
                .any(|p| p.pid == me && p.resident_bytes > 0)
        );
        assert!(
            s2.top_processes
                .windows(2)
                .all(|w| w[0].cpu_percent >= w[1].cpu_percent)
        );
    }

    #[test]
    fn page_scale_converts_process_pages_to_kernel_pages() {
        // SAFETY: sysconf has no preconditions.
        let process_page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        let kernel_page = platform::sysctl_i32("vm.pagesize").unwrap() as u64;
        assert_eq!(platform::vm_page_scale() * process_page, kernel_page);
        if platform::arch_info().rosetta_translated == Some(true) {
            assert!(platform::vm_page_scale() > 1);
        }
    }

    #[test]
    fn interval_is_clamped_to_sysinfo_minimum() {
        let m = Monitor::new(Duration::from_millis(1), 1, ProcessSort::Cpu);
        assert!(m.interval >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    }
}
