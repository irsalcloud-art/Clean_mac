//! Command-line parsing and command dispatch.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

use crate::cleaner::{self, CategoryId, CleanContext};
use crate::errors::{MacCleanError, Result, exit};
use crate::monitor::{self, Monitor, ProcessSort};
use crate::platform;
use crate::report::{self, FullReport, OutputFormat};
use crate::scanner::{self, ScanOptions, SizeMetric};

#[derive(Debug, Parser)]
#[command(
    name = "macclean",
    version,
    about = "MacClean — storage analyzer, safe cache cleaner and system monitor for macOS",
    long_about = "MacClean — storage analyzer, safe cache cleaner and system monitor for macOS.\n\n\
        MacClean is read-only unless you run `clean --interactive` and confirm.\n\
        It never uses sudo, never follows symbolic links, and only cleans \
        explicitly supported cache categories.",
    after_help = "Exit codes: 0 success, 1 error, 2 usage error, 3 partial result \
        (scan incomplete or some items not removed), 4 cancelled, 5 refused by safety policy."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show macOS version, architecture, memory, disk and uptime.
    Status(FormatArg),
    /// Rank the largest files and directories under the given paths (read-only).
    Analyze(AnalyzeArgs),
    /// Find (and optionally delete) supported caches. Dry run by default.
    Clean(CleanArgs),
    /// Show CPU, memory, disk and top processes, refreshed at an interval.
    Monitor(MonitorArgs),
    /// Produce a combined status, cleanup dry-run and optional analysis report.
    Report(ReportArgs),
}

#[derive(Debug, Args)]
pub struct FormatArg {
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct AnalyzeArgs {
    /// Directories or files to analyze. Defaults to the current directory.
    pub paths: Vec<PathBuf>,
    /// Number of largest files and directories to show.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub top: u16,
    /// Size used for ranking.
    #[arg(long, value_enum, default_value_t = SizeMetric::Allocated)]
    pub sort: SizeMetric,
    /// Also descend into directories on other filesystems (mounted volumes).
    #[arg(long)]
    pub cross_mounts: bool,
    #[command(flatten)]
    pub format: FormatArg,
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    /// Category to clean; repeat for several. Defaults to all categories.
    #[arg(long = "category", value_enum)]
    pub categories: Vec<CategoryId>,
    /// Only show what would be deleted (the default).
    #[arg(long, conflicts_with = "interactive")]
    pub dry_run: bool,
    /// Show the plan, ask for confirmation, then delete.
    #[arg(long)]
    pub interactive: bool,
    #[command(flatten)]
    pub format: FormatArg,
}

#[derive(Debug, Args)]
pub struct MonitorArgs {
    /// Seconds between samples.
    #[arg(long, default_value_t = 2,
          value_parser = clap::value_parser!(u64).range(monitor::MIN_INTERVAL_SECS..=monitor::MAX_INTERVAL_SECS))]
    pub interval: u64,
    /// Number of samples to take; 0 runs until interrupted (Ctrl-C).
    #[arg(long, default_value_t = 1)]
    pub count: u32,
    /// Number of processes to list.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u16).range(1..=500))]
    pub top: u16,
    /// Process ranking.
    #[arg(long, value_enum, default_value_t = ProcessSort::Cpu)]
    pub sort: ProcessSort,
    /// Output format. JSON emits one object per line per sample.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct ReportArgs {
    /// Also analyze these paths and include the results.
    #[arg(long = "path")]
    pub paths: Vec<PathBuf>,
    /// Number of largest entries to include in the analysis.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub top: u16,
    #[command(flatten)]
    pub format: FormatArg,
}

/// Reads one line and accepts only an exact "yes".
pub fn confirm(input: &mut dyn BufRead, out: &mut dyn Write, prompt: &str) -> io::Result<bool> {
    write!(out, "{prompt}")?;
    out.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(false);
    }
    Ok(line.trim_end_matches(['\r', '\n']) == "yes")
}

fn scan_options(
    paths: Vec<PathBuf>,
    top: u16,
    sort: SizeMetric,
    cross: bool,
) -> Result<ScanOptions> {
    let roots = if paths.is_empty() {
        vec![std::env::current_dir().map_err(|e| MacCleanError::io(".", e))?]
    } else {
        let cwd = std::env::current_dir().map_err(|e| MacCleanError::io(".", e))?;
        paths
            .into_iter()
            .map(|p| if p.is_absolute() { p } else { cwd.join(p) })
            .collect()
    };
    Ok(ScanOptions {
        roots,
        top: usize::from(top),
        sort_by: sort,
        cross_mounts: cross,
    })
}

fn categories_or_all(c: Vec<CategoryId>) -> Vec<CategoryId> {
    if c.is_empty() {
        CategoryId::ALL.to_vec()
    } else {
        let mut v = c;
        v.dedup();
        v
    }
}

fn run_clean(args: CleanArgs, out: &mut dyn Write) -> Result<u8> {
    let ctx = CleanContext::detect()?;
    let cats = categories_or_all(args.categories);
    let mut plan = cleaner::plan(&cats, &ctx);

    if !args.interactive {
        match args.format.format {
            OutputFormat::Text => report::render_plan(out, &plan)?,
            OutputFormat::Json => report::write_json(out, &plan)?,
        }
        return Ok(exit::SUCCESS);
    }

    plan.dry_run = false;
    // The plan and prompt go to stderr so stdout carries only the result.
    let mut err = io::stderr().lock();
    report::render_plan(&mut err, &plan)?;
    if !plan.has_candidates() {
        writeln!(err, "\nNothing to delete.")?;
        return Ok(exit::SUCCESS);
    }
    if !platform::stdin_is_tty() {
        return Err(MacCleanError::NotInteractive);
    }
    let total: u64 = plan
        .categories
        .iter()
        .map(|c| c.estimate.allocated_bytes)
        .sum();
    let count: usize = plan.categories.iter().map(|c| c.candidates.len()).sum();
    let prompt = format!(
        "\nDelete {count} candidate(s), about {}? Only the candidates listed above will be touched.\n\
         Type 'yes' to delete, anything else to cancel: ",
        report::human_bytes(total)
    );
    if !confirm(&mut io::stdin().lock(), &mut err, &prompt)? {
        return Err(MacCleanError::Cancelled);
    }
    drop(err);
    let outcome = cleaner::execute(&plan, &ctx);
    match args.format.format {
        OutputFormat::Text => report::render_outcome(out, &outcome)?,
        OutputFormat::Json => report::write_json(out, &outcome)?,
    }
    Ok(if outcome.is_partial() {
        exit::PARTIAL
    } else {
        exit::SUCCESS
    })
}

fn run_monitor(args: MonitorArgs, out: &mut dyn Write) -> Result<u8> {
    let mut m = Monitor::new(
        Duration::from_secs(args.interval),
        usize::from(args.top),
        args.sort,
    );
    let clear = args.count != 1 && args.format == OutputFormat::Text && platform::stdout_is_tty();
    let mut taken: u32 = 0;
    loop {
        let sample = m.next_sample();
        match args.format {
            OutputFormat::Text => {
                if clear {
                    write!(out, "\x1b[2J\x1b[H")?;
                } else if taken > 0 {
                    writeln!(out)?;
                }
                report::render_sample(out, &sample)?;
            }
            OutputFormat::Json if args.count == 1 => report::write_json(out, &sample)?,
            OutputFormat::Json => report::write_json_line(out, &sample)?,
        }
        out.flush()?;
        taken = taken.saturating_add(1);
        if args.count != 0 && taken >= args.count {
            return Ok(exit::SUCCESS);
        }
    }
}

fn dispatch(cli: Cli, out: &mut dyn Write) -> Result<u8> {
    match cli.command {
        Command::Status(f) => {
            let s = report::collect_status();
            match f.format {
                OutputFormat::Text => report::render_status(out, &s)?,
                OutputFormat::Json => report::write_json(out, &s)?,
            }
            Ok(exit::SUCCESS)
        }
        Command::Analyze(a) => {
            let r = scanner::scan(scan_options(a.paths, a.top, a.sort, a.cross_mounts)?);
            match a.format.format {
                OutputFormat::Text => report::render_scan(out, &r)?,
                OutputFormat::Json => report::write_json(out, &r)?,
            }
            Ok(if r.incomplete {
                exit::PARTIAL
            } else {
                exit::SUCCESS
            })
        }
        Command::Clean(c) => run_clean(c, out),
        Command::Monitor(m) => run_monitor(m, out),
        Command::Report(r) => {
            let status = report::collect_status();
            let ctx = CleanContext::detect()?;
            let plan = cleaner::plan(&CategoryId::ALL, &ctx);
            let analysis = if r.paths.is_empty() {
                None
            } else {
                Some(scanner::scan(scan_options(
                    r.paths,
                    r.top,
                    SizeMetric::Allocated,
                    false,
                )?))
            };
            let incomplete = analysis.as_ref().is_some_and(|a| a.incomplete);
            let full = FullReport::new(status, plan, analysis);
            match r.format.format {
                OutputFormat::Text => report::render_full(out, &full)?,
                OutputFormat::Json => report::write_json(out, &full)?,
            }
            Ok(if incomplete {
                exit::PARTIAL
            } else {
                exit::SUCCESS
            })
        }
    }
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    match dispatch(cli, &mut out) {
        Ok(code) => ExitCode::from(code),
        Err(MacCleanError::Output(e)) if e.kind() == io::ErrorKind::BrokenPipe => {
            ExitCode::from(exit::SUCCESS)
        }
        Err(e) => {
            let _ = out.flush();
            eprintln!("macclean: {e}");
            e.exit_code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn confirm_accepts_only_exact_yes() {
        for (input, expected) in [
            ("yes\n", true),
            ("yes", true),
            ("YES\n", false),
            ("y\n", false),
            ("no\n", false),
            (" yes\n", false),
            ("\n", false),
            ("", false),
        ] {
            let mut out = Vec::new();
            let got = confirm(&mut input.as_bytes(), &mut out, "? ").unwrap();
            assert_eq!(got, expected, "input {input:?}");
            assert_eq!(out, b"? ");
        }
    }

    #[test]
    fn interactive_and_dry_run_conflict() {
        assert!(Cli::try_parse_from(["macclean", "clean", "--dry-run", "--interactive"]).is_err());
    }
}
