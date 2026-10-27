//! End-to-end tests of the `macclean` binary. A temporary directory stands in
//! for `$HOME`, so cleanup commands only ever see synthetic fixtures.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        for (rel, len) in [
            (".npm/_cacache/content-v2/sha512/aa/bb/blob", 4000usize),
            (".npm/_cacache/index-v5/cc/dd/entry", 100),
            (".npm/_logs/keep.log", 10),
            ("Documents/keep.txt", 10),
        ] {
            let p = home.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, vec![b'z'; len]).unwrap();
        }
        Env { _tmp: tmp, home }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_macclean"));
        c.args(args)
            .env("HOME", &self.home)
            .env("npm_config_cache", self.home.join(".npm"))
            .env("MACCLEAN_TEST_ASSUME_NO_PACKAGE_MANAGER", "1")
            .current_dir(&self.home)
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn assert_untouched(&self) {
        for rel in [
            ".npm/_cacache/content-v2/sha512/aa/bb/blob",
            ".npm/_cacache/index-v5/cc/dd/entry",
            ".npm/_logs/keep.log",
            "Documents/keep.txt",
        ] {
            assert!(self.home.join(rel).exists(), "{rel} must still exist");
        }
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("exited normally")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("invalid JSON ({e}):\n{}", stdout(o)))
}

#[test]
fn help_and_version() {
    let e = Env::new();
    let h = e.run(&["--help"]);
    assert_eq!(code(&h), 0);
    let text = stdout(&h);
    assert!(text.contains("MacClean"));
    for sub in ["status", "analyze", "clean", "monitor", "report"] {
        assert!(text.contains(sub), "help lists {sub}");
    }
    let v = e.run(&["--version"]);
    assert_eq!(code(&v), 0);
    assert_eq!(
        stdout(&v).trim(),
        format!("macclean {}", env!("CARGO_PKG_VERSION"))
    );
    for sub in ["status", "analyze", "clean", "monitor", "report"] {
        assert_eq!(code(&e.run(&[sub, "--help"])), 0);
    }
}

#[test]
fn invalid_arguments_exit_with_usage_code() {
    let e = Env::new();
    for args in [
        vec!["bogus"],
        vec!["clean", "--category", "everything"],
        vec!["clean", "--category", "../../Documents"],
        vec!["clean", "--dry-run", "--interactive"],
        vec!["analyze", "--top", "0"],
        vec!["analyze", "--top", "abc"],
        vec!["monitor", "--interval", "0"],
        vec!["status", "--format", "xml"],
        vec!["clean", "--path", "/"],
        vec!["analyze", "--exclude", ""],
        vec!["analyze", "--max-depth", "-1"],
        vec!["analyze", "--max-depth", "abc"],
        vec!["monitor", "--tui"],
        vec!["monitor", "--tui", "--count", "2"],
        vec!["monitor", "--tui", "--format", "json"],
    ] {
        let o = e.run(&args);
        assert_eq!(code(&o), 2, "{args:?}");
    }
    e.assert_untouched();
}

#[test]
fn status_json_is_valid() {
    let e = Env::new();
    let o = e.run(&["status", "--format", "json"]);
    assert_eq!(code(&o), 0);
    let v = json(&o);
    assert!(v["memory"]["total_bytes"].as_u64().unwrap() > 0);
    assert!(v["os"]["product_version"].is_string());
    assert!(v["arch"]["binary_arch"].is_string());
    assert!(v["disk"]["total_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn analyze_json_and_exit_codes() {
    let e = Env::new();
    let o = e.run(&["analyze", ".npm", "--top", "2", "--format", "json"]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let v = json(&o);
    assert_eq!(v["totals"]["files"], 3);
    assert_eq!(v["totals"]["logical_bytes"], 4110);
    assert_eq!(v["top_files"].as_array().unwrap().len(), 2);
    assert_eq!(v["incomplete"], false);

    let missing = e.run(&["analyze", "does-not-exist"]);
    assert_eq!(code(&missing), 3);
    assert!(stdout(&missing).contains("INCOMPLETE SCAN"));
    e.assert_untouched();
}

#[test]
fn clean_defaults_to_dry_run_and_deletes_nothing() {
    let e = Env::new();
    for args in [
        vec!["clean"],
        vec!["clean", "--dry-run"],
        vec!["clean", "--category", "npm-cache", "--dry-run"],
    ] {
        let o = e.run(&args);
        assert_eq!(
            code(&o),
            0,
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let text = stdout(&o);
        assert!(text.contains("dry run — nothing will be deleted"));
        assert!(text.contains("content-v2"), "{text}");
        e.assert_untouched();
    }
    let o = e.run(&["clean", "--category", "npm-cache", "--format", "json"]);
    let v = json(&o);
    assert_eq!(v["dry_run"], true);
    let cat = &v["categories"][0];
    assert_eq!(cat["category"], "npm-cache");
    assert_eq!(cat["state"], "ready");
    assert_eq!(cat["candidates"].as_array().unwrap().len(), 2);
    assert_eq!(cat["estimate"]["logical_bytes"], 4100);
    e.assert_untouched();
}

#[test]
fn interactive_clean_without_terminal_is_refused() {
    let e = Env::new();
    let o = e.run(&["clean", "--category", "npm-cache", "--interactive"]);
    assert_eq!(code(&o), 5, "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("interactive terminal"));
    e.assert_untouched();
}

#[test]
fn piped_yes_does_not_bypass_confirmation() {
    let e = Env::new();
    let mut child = e
        .cmd(&["clean", "--category", "npm-cache", "--interactive"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(b"yes\n").unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(code(&o), 5);
    e.assert_untouched();
}

#[test]
fn running_npm_refuses_npm_cache_cleanup() {
    use std::os::unix::process::CommandExt;
    let e = Env::new();
    // A process that looks like `npm install` to `ps`: npm rewrites its
    // argv[0] through process.title in the same way.
    let mut npm = Command::new("/bin/sleep")
        .arg0("npm install")
        .arg("60")
        .spawn()
        .unwrap();
    let clean = |args: &[&str]| {
        let mut c = e.cmd(args);
        c.env_remove("MACCLEAN_TEST_ASSUME_NO_PACKAGE_MANAGER");
        c.output().unwrap()
    };
    let dry = clean(&["clean", "--category", "npm-cache", "--format", "json"]);
    let interactive = clean(&["clean", "--category", "npm-cache", "--interactive"]);
    npm.kill().unwrap();
    npm.wait().unwrap();

    let v = json(&dry);
    assert_eq!(v["categories"][0]["state"], "refused");
    let reason = v["categories"][0]["state_reason"].as_str().unwrap();
    assert!(reason.contains("npm install"), "{reason}");
    let err = String::from_utf8_lossy(&interactive.stderr);
    assert!(err.contains("REFUSED"), "{err}");
    assert!(err.contains("Nothing to delete"), "{err}");
    assert_eq!(code(&interactive), 0);
    e.assert_untouched();
}

#[test]
fn analyze_skips_cloud_folders_unless_included() {
    let e = Env::new();
    let cloud = e.home.join("Library/CloudStorage/Dropbox/big");
    fs::create_dir_all(cloud.parent().unwrap()).unwrap();
    fs::write(&cloud, vec![b'c'; 3000]).unwrap();

    let o = e.run(&["analyze", "--format", "json"]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let v = json(&o);
    assert_eq!(
        v["cloud_folders_skipped"][0],
        e.home.join("Library/CloudStorage").display().to_string()
    );
    assert_eq!(v["totals"]["logical_bytes"], 4120);
    assert_eq!(v["incomplete"], false);
    let text = stdout(&e.run(&["analyze"]));
    assert!(text.contains("--include-cloud"), "{text}");

    let o = e.run(&["analyze", "--include-cloud", "--format", "json"]);
    let v = json(&o);
    assert_eq!(v["cloud_folders_skipped"].as_array().unwrap().len(), 0);
    assert_eq!(v["totals"]["logical_bytes"], 7120);
    e.assert_untouched();
}

#[test]
fn monitor_snapshot_and_stream() {
    let e = Env::new();
    let o = e.run(&[
        "monitor",
        "--interval",
        "1",
        "--count",
        "1",
        "--format",
        "json",
        "--top",
        "3",
    ]);
    assert_eq!(code(&o), 0);
    let v = json(&o);
    assert!(v["cpu"]["logical_cores"].as_u64().unwrap() > 0);
    assert_eq!(v["top_processes"].as_array().unwrap().len(), 3);

    let o = e.run(&[
        "monitor",
        "--interval",
        "1",
        "--count",
        "2",
        "--format",
        "json",
    ]);
    assert_eq!(code(&o), 0);
    let lines: Vec<_> = stdout(&o).lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 2);
    for l in lines {
        serde_json::from_str::<serde_json::Value>(&l).unwrap();
    }

    let t = e.run(&["monitor", "--interval", "1", "--count", "1"]);
    assert!(stdout(&t).contains("CPU:"));
}

#[test]
fn report_json_is_valid() {
    let e = Env::new();
    let o = e.run(&["report", "--format", "json", "--path", ".npm"]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let v = json(&o);
    assert!(v["status"].is_object());
    assert_eq!(v["cleanup_plan"]["dry_run"], true);
    assert_eq!(v["analysis"]["totals"]["files"], 3);
    let text = e.run(&["report"]);
    assert_eq!(code(&text), 0);
    assert!(stdout(&text).contains("Cleanup plan"));
    e.assert_untouched();
}

#[test]
fn analyze_default_is_current_directory_only() {
    let e = Env::new();
    let sub = e.home.join("Documents");
    let o = Command::new(env!("CARGO_BIN_EXE_macclean"))
        .args(["analyze", "--format", "json"])
        .current_dir(&sub)
        .output()
        .unwrap();
    let v = json(&o);
    assert_eq!(v["roots"][0].as_str().map(Path::new), Some(sub.as_path()));
    assert_eq!(v["totals"]["files"], 1);
}
