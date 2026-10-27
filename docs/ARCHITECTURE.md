# Architecture

MacClean is a single Rust crate, built as a library (`src/lib.rs`) with a thin binary (`src/main.rs` calls `cli::run`). It only compiles on macOS.

```
main.rs ─► cli ─┬─► scanner ─────────────► platform::fsops (read)
                ├─► cleaner ─► policy, npm_config
                │      └───────────────────► platform::fsops (read + delete)
                ├─► monitor ─► sysinfo, platform (sysctl)
                └─► report  (text / JSON rendering of every result)
                    errors  (error type → exit code)
```

## Modules

| Module | Role | Writes to disk? |
|---|---|---|
| `cli` | clap argument parsing, command dispatch, the `yes` confirmation prompt, exit codes | No |
| `scanner` | `analyze`: walks the given roots with `lstat` semantics and keeps the top N files and directories in bounded heaps | No |
| `cleaner` | Category definitions, root resolution, classification, planning (`plan`) and guarded deletion (`execute`) | Only in `execute` |
| `npm_config` | Finds npm's cache directory from the environment and the user, global and builtin npmrc files in npm's order, without running npm | No |
| `policy` | Protected locations, sensitive subtrees, `validate_category_root`, the `Risk`/`Verdict` types | No |
| `platform` | sysctl, `statfs`, uid, home directory, TTY checks, architecture and Rosetta detection, reading app preferences | No |
| `platform::fsops` | Descriptor-relative primitives: `open_dir_nofollow`, `open_dir_at`, `stat_at`, `read_dir_names`, `unlink_at`, `rmdir_at` | `unlink_at` and `rmdir_at` only |
| `monitor` | CPU, memory, disk and process samples (via `sysinfo`), with a correction for the page size under Rosetta | No |
| `report` | Human-readable text and JSON output for status, scans, plans, outcomes and samples | No |
| `errors` | `MacCleanError` and the exit-code table (0 success, 1 error, 2 usage, 3 partial, 4 cancelled, 5 refused) | No |

## The three stages of `clean`

Cleanup is split so that each stage can be checked on its own. Only the last stage deletes anything.

### 1. Resolve and validate the root

`cleaner::resolve_root` finds each category's root directory. For `npm-cache` it refuses while npm, npx, yarn or pnpm is running (`running_package_manager`), then asks `npm_config::detect_npm_cache_dir`, which follows npm's order: environment, user npmrc, global npmrc, builtin npmrc, `~/.npm`. A project `.npmrc` is only reported, as a note on the plan. For `xcode-derived-data` it refuses while Xcode is running, then reads Xcode's `IDECustomDerivedDataLocation` preference (`platform::read_app_preference`, read-only CFPreferences) and turns it into a root with `derived_data_location`, falling back to `~/Library/Developer/Xcode/DerivedData`. `finish_resolution` refuses a root that is a symlink or not a directory, and canonicalizes it. Then `policy::validate_category_root` checks it:

* it must be an absolute path with no `.` or `..` components
* it must lie strictly inside the canonical home directory
* it must not be, or contain, a protected location (`/`, `/System`, `/Users`, home, `~/Library`, `~/Documents`, …)
* it must not lie inside a sensitive subtree (Keychains, Mail, `.ssh`, …) or inside a `.git` directory

All comparisons go through `Path::starts_with`, so they compare path components, never string prefixes.

### 2. Plan (read-only)

`cleaner::plan` → `plan_category`:

1. Open the root with `open_dir_nofollow`, which opens every path component with `O_NOFOLLOW`, then record its device and inode (`root_id`) and check that the current user owns it.
2. List the root's direct children and classify each one with `classify`. The result is `Safe`, `Review` (not recognized, kept) or `Forbidden` (symlink, or owned by another user).
3. Measure each `Safe` child with the descriptor-relative `Walker` in `Measure` mode. Its totals become the estimate.

The result is a `CleanPlan`, which `report` renders as text or JSON. Without `--interactive`, the process stops here.

### 3. Execute (guarded)

`cli::run_clean` calls `cleaner::execute` only when all of these hold:

* the plan has candidates
* stdin is a TTY (otherwise `NotInteractive`, exit code 5)
* the user typed exactly `yes` (otherwise `Cancelled`, exit code 4)

`execute_category` then repeats the safety checks:

1. Resolve the root again from scratch, and refuse if the path differs from the plan.
2. Open it again with `O_NOFOLLOW`, and refuse unless the device, inode and owner match `root_id`.
3. For each candidate: check that it is a direct child of the root, `stat_at` it, and require the same identity and type as in the plan. Then classify it again.
4. Walk it with `Walker` in `Delete` mode. Each entry is skipped and reported if it changed after the plan (mtime or ctime later than the plan's timestamp), is owned by another user, has more than one hard link, has immutable or append-only flags, is a special file, is on another filesystem, or is nested more than 128 levels deep. Everything else is removed with `unlink_at` / `rmdir_at` relative to its parent descriptor.

The `CleanOutcome` counts only successful removals, and lists every skip and failure (`MAX_RECORDED_ITEMS` = 100 per category). Any leftover makes the run partial (exit code 3).

## Scanner

`scanner::scan` walks each root without following symlinks. By default it doesn't cross into other filesystems, and it lists the mount points it skipped. It counts hard-linked files once and keeps only the top N entries in heaps, so memory stays flat as the tree grows. Paths it could not read are recorded with a `SkipReason` and make the result partial.

## Monitor

`monitor::Monitor` wraps `sysinfo` with only the `system` feature. Under Rosetta 2, macOS reports 4 KiB pages to the translated process while the kernel counts 16 KiB pages, so `platform::vm_page_scale` (from `vm.pagesize`) corrects the memory totals. The monitor never sends signals to processes.

## Testing

* **Unit tests** sit next to the code (`#[cfg(test)]` modules) and cover parsing, policy and classification.
* **`tests/cleaner.rs`** builds a fake home directory in a `tempfile` directory and passes it in through `CleanContext { home, uid, npm_cache: NpmCacheSource::Explicit(..), xcode_running: Some(false), package_manager_running: Some(false), derived_data: DerivedDataSetting::Given(..) }`. This means no test touches the real home directory. The tests cover the attack and race scenarios listed in the README.
* **`tests/cli.rs`** runs the compiled binary and checks arguments, exit codes, JSON validity, and the refusal without a TTY. It sets `MACCLEAN_TEST_ASSUME_NO_PACKAGE_MANAGER` so a package manager running on the test machine does not refuse the npm-cache fixtures. Only debug builds read that variable. One test unsets it and starts a fake `npm install` process to check the refusal end to end.
* **`tests/properties.rs`** holds the property tests for `policy::validate_category_root` and `fsops::check_entry_name` (proptest, 256 cases by default) and a stress test that mutates the cache from another thread while `cleaner::execute` runs. Each checks that nothing outside its temporary directories changes.
* **`scripts/check.sh`** runs formatting, clippy, the tests and release builds. **`scripts/smoke.sh`** runs every read-only command against a built binary. CI runs both on arm64 and on Intel, and under Rosetta 2 (`.github/workflows/ci.yml`).
* **Releases** come from `.github/workflows/release.yml`: a `v<version>` tag builds and tests each architecture on its own runner, packages it with `scripts/package.sh`, and publishes the archives, `SHA256SUMS` and notes from `scripts/release-notes.sh`. See [RELEASING.md](RELEASING.md).

## Build settings

* `.cargo/config.toml` sets `MACOSX_DEPLOYMENT_TARGET=11.0`. CI checks that `otool -l` reports `minos 11.0` for both architectures. The documented supported minimum is macOS 15, the oldest version with a recorded test run.
* The release profile uses LTO, one codegen unit and stripped symbols.
