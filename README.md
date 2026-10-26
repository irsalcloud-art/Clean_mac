# MacClean

MacClean is a small command-line tool for macOS with three jobs:

1. **Storage analyzer**: finds the largest files and directories under the paths you choose. It only reads.
2. **Safe cleaner**: removes a short list of well-understood developer caches. It does a dry run by default, and deletes only after you confirm.
3. **System monitor**: shows CPU, memory, disk, uptime and the top processes. It only reads.

MacClean needs no administrator rights. It never runs `sudo` and never follows symbolic links. It sends no telemetry and makes no network requests. It runs no daemon or helper. It does not "free RAM", kill processes or change system settings. It does not promise to make your Mac faster. It reports what it measured.

## Current status (v0.1.0)

| Feature | Status |
|---|---|
| `status`: macOS version, architecture, memory, disk, uptime | Implemented, tested |
| `analyze`: top files/directories, logical vs allocated size, JSON | Implemented, tested |
| `clean`: dry-run plan for `npm-cache`, `xcode-derived-data` | Implemented, tested |
| `clean --interactive`: confirmed, revalidated deletion | Implemented, tested (including a real-pty run) |
| `monitor`: snapshot and repeated samples, text/NDJSON | Implemented, tested |
| `report`: combined status, cleanup plan and optional analysis | Implemented, tested |
| Full-screen live TUI for `monitor` | Planned |
| More cache categories (Homebrew, pip, Gradle, CocoaPods, …) | Planned, each needs its own policy and tests first |
| Universal (fat) binary | Planned, after the x86_64 build is validated on Intel hardware |
| Custom Xcode DerivedData location | Not supported (it is refused) |
| Arbitrary path deletion | Deliberately not supported |

The planned work, its priorities and goals are in [ROADMAP.md](ROADMAP.md).

## Compatibility

Only the combinations below have a recorded test run. CI runs the full test suite and `scripts/smoke.sh` (every command, `clean` as a dry run) on each pull request.

| Hardware | macOS | How it was tested |
|---|---|---|
| Apple Silicon (Apple M3) | 27.0.1 (26A434) | By hand: test suite, smoke test, interactive cleanup in a real terminal |
| Apple Silicon (Apple M1, virtual) | 15.7 | CI (`macos-15` runner): test suite and smoke test |
| Intel (Core i7-8700B) | 15.7 | CI (`macos-15-intel` runner, a VM on Intel Mac hardware): native x86_64 test suite and smoke test |
| x86_64 binary under Rosetta 2 | 27.0.1 and 15.7 | By hand on the M3, and in CI on the `macos-15` runner: test suite and smoke test |

**Supported minimum: macOS 15 (Sequoia).** Binaries are still built with `MACOSX_DEPLOYMENT_TARGET=11.0` and CI checks that the Mach-O `minos` is 11.0, so macOS 11–14 will load them. Nothing has been tested there, so treat 11–14 as unsupported until someone records a run (run `sh scripts/smoke.sh` and the test suite, then open an issue with the output).

Interactive cleanup (`clean --interactive`) has only been run by hand on the M3. CI runs `clean` as a dry run only.

The binary links only system libraries (libSystem, CoreFoundation, IOKit, libiconv). It does not depend on a particular Mac model or home directory path. Under Rosetta, `status` reports that the binary is translated.

## Build and install

You need a Rust toolchain (tested with Rust 1.95.0, edition 2024). Build with Cargo from any shell, for example Rush or zsh:

```bash
cargo build --release
```

The binary is `target/release/macclean`. To install it for the current user only:

```bash
cargo install --path .
```

That puts `macclean` in `~/.cargo/bin`. Architecture-specific release builds:

```bash
rustup target add x86_64-apple-darwin
```

```bash
cargo build --release --target x86_64-apple-darwin
```

```bash
cargo build --release --target aarch64-apple-darwin
```

A successful cross-compile does **not** prove the binary runs correctly on that architecture. Test it on real Intel hardware before you ship it. On an Intel Mac, or an Apple Silicon Mac with Rosetta 2 installed, run the test suite as x86_64 code:

```bash
cargo test --target x86_64-apple-darwin
```

To smoke-test a built binary on a given Mac (read-only, deletes nothing):

```bash
sh scripts/smoke.sh target/release/macclean
```

### Uninstall

```bash
cargo uninstall macclean
```

If you copied the binary somewhere by hand, delete that copy. MacClean creates no configuration files, caches, launch agents or other state.

## Usage

```text
macclean --help
macclean --version
macclean status [--format text|json]
macclean analyze [PATHS]... [--top N] [--sort allocated|logical] [--cross-mounts] [--format text|json]
macclean clean [--category npm-cache|xcode-derived-data]... [--dry-run | --interactive] [--format text|json]
macclean monitor [--interval SECS] [--count N] [--top N] [--sort cpu|memory] [--format text|json]
macclean report [--path PATH]... [--top N] [--format text|json]
```

Examples:

```bash
macclean status
```

```bash
macclean analyze ~/Downloads ~/Library/Caches --top 20
```

```bash
macclean analyze --top 20 --format json > scan.json
```

```bash
macclean clean --dry-run
```

```bash
macclean clean --category npm-cache --dry-run
```

```bash
macclean clean --category npm-cache --interactive
```

```bash
macclean monitor --interval 2 --count 0 --sort memory
```

```bash
macclean report --format json --path ~/Projects
```

Notes:

* `analyze` with no paths scans **the current directory only**. It never scans the whole disk unless you pass `/` yourself. By default it stays on one filesystem; mount points it did not enter are listed.
* `monitor --count 0` runs until you press Ctrl-C. With `--format json` and more than one sample, it prints one JSON object per line.
* `clean` without `--interactive` is always a dry run. `--dry-run` and `--interactive` cannot be combined.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Runtime error |
| 2 | Invalid usage (bad flag, category or value) |
| 3 | Partial result: the scan skipped paths, or the cleanup left some planned items in place |
| 4 | Cancelled at the confirmation prompt (nothing deleted) |
| 5 | Refused by safety policy, e.g. `--interactive` without a terminal (nothing deleted) |

## Sizes: logical, allocated, reclaimable

* **Logical size** is the file length (`st_size`).
* **Allocated size** is the storage the file occupies now (`st_blocks × 512`). It can be smaller (sparse or compressed files) or larger (block rounding).
* **Estimated reclaimable** in a cleanup plan is the allocated size of the files that would be deleted. It is an upper bound. APFS clones, hard links, local Time Machine snapshots and files that are still open can all mean less space actually comes back.
* After a cleanup, MacClean reports the allocated bytes of the files it actually removed. It also reports the *observed* change in available volume space, which other activity on the Mac affects as well.

Disk "available" comes from `statfs(2)` and excludes APFS purgeable space, so Finder may show a larger number. Sizes use decimal units (1 GB = 10⁹ bytes), as Finder does. Hard-linked files are counted once.

## Cleanup categories

### `npm-cache`

* **What it deletes:** `content-v2`, `index-v5` and `tmp` inside npm's `_cacache` directory. This is the content that `npm cache clean --force` removes. npm downloads packages again when it needs them.
* **How the location is found** (npm itself is not run, because running it writes logs and may contact the registry):
  1. the `npm_config_cache` / `NPM_CONFIG_CACHE` environment variable,
  2. `cache=` in the user npmrc (`$NPM_CONFIG_USERCONFIG` or `~/.npmrc`), with `~/` expanded,
  3. the default `~/.npm`.

  Values that use `${VAR}` expansion, or relative paths, are refused. Project-level and global npmrc files are not read.
* **What it never touches:** `_logs`, `_npx`, `_update-notifier-last-checked`, and any entry in `_cacache` it does not recognize. Those are listed as "review". If `_cacache` contains neither `content-v2` nor `index-v5`, the whole category is refused.

### `xcode-derived-data`

* **What it deletes:** entries directly inside `~/Library/Developer/Xcode/DerivedData` that are either per-project folders named `<Project>-<28 lowercase letters>` (Xcode's naming scheme) or `ModuleCache.noindex`, `SymbolCache.noindex` or `CompilationCache.noindex`. Xcode rebuilds them on the next build, which can take a while.
* **When it refuses:** while `Xcode`, `xcodebuild`, `XCBBuildService` or `SWBBuildService` is running, and when DerivedData is a symbolic link or has been moved.
* **What it never touches:** Archives, CoreSimulator devices, and any entry with an unrecognized name.

## Security model

Filesystem safety is treated as a requirement that blocks a release.

* **Three separate stages.** The scanner (`analyze`) only reads. Planning (`clean`, dry run) only reads. Deletion happens only in `cleaner::execute`, which the CLI calls only after you type exactly `yes` at an interactive terminal. Piping `yes` into MacClean does not work: stdin must be a TTY, otherwise it exits with code 5.
* **Fixed allowlist.** Only the two category roots above can be cleaned. There is no way to delete an arbitrary path.
* **Policy checks on each root.** A root must be an absolute path without `..`. It must sit strictly inside your home directory. It must not be, or contain, a protected location (`/`, `/System`, `/Users`, home itself, `~/Library`, `~/Documents`, …). It must not lie inside a sensitive area (Keychains, Application Support, Containers, Mail, Messages, Safari, `.ssh`, `.gnupg`, `.aws`, `.config`, `.git`, Documents, Desktop, Pictures, …). These comparisons use path components, never string prefixes.
* **Symbolic links.** Category roots and candidates that are symlinks are refused. During deletion every directory is opened with `O_NOFOLLOW` relative to its parent descriptor, so a component swapped for a symlink after planning makes the open fail. A symlink *inside* a cache is removed as a link entry (`unlinkat`), and its target is never touched.
* **Revalidation just before deletion.** MacClean re-resolves the category root and checks it is still the same path. It re-opens the root component by component with `O_NOFOLLOW`, then checks the root's device and inode against the plan. It re-checks each candidate's identity, type, owner and classification. Anything that changed is reported as "not touched".
* **Files are skipped and reported, not deleted, if they:**
  * changed after the plan was created (based on the later of mtime and ctime),
  * are owned by another user,
  * have more than one hard link,
  * carry immutable or append-only flags,
  * are special files,
  * are on another filesystem,
  * are nested more than 128 levels deep.
* **Accurate reporting.** Counts and bytes include only entries whose `unlink` succeeded. Every failure and skip is listed (up to 100 per category, with totals). A run that leaves anything planned in place exits with code 3.
* **No privileges.** No `sudo`, no authorization prompts, no privileged helper and no daemon.

### Known limitations of the security model

* Descriptor-relative deletion stops redirection through symlinks. It cannot stop a process running as **your own user** from moving a directory MacClean is in the middle of deleting to somewhere else. Such a process could delete those files itself anyway.
* If a file is swapped for another regular file between the per-file check and the `unlink`, MacClean removes the new directory entry. That entry is still inside the verified cache directory.
* Detecting whether npm is running is not implemented. Files npm writes after the plan was created are skipped by the change-time rule, but do not run a cleanup while `npm install` is running.
* npm configuration in project-level or global npmrc files, and a custom Xcode DerivedData location set in Xcode preferences, are not detected.
* No external security audit has been performed.

## System monitor

* **CPU:** average use across all logical cores over the sample interval, plus load averages. Per-process CPU is measured per core and can exceed 100%.
* **Memory:** "used" is app + wired + compressed memory (close to Activity Monitor's "Memory Used"). "Available" is active + inactive + free + speculative pages. Process memory is the **resident set size (RSS)**. Shared pages are counted in every process that maps them, so process values do not add up to system usage, and they differ from Activity Monitor's "Memory" column (physical footprint).
* Under Rosetta 2, macOS shows a translated process 4 KiB pages while the kernel counts 16 KiB pages. MacClean scales used and available memory by `vm.pagesize` so they match the native build. Without that correction (the `sysinfo` default), both values came out 4x too small.
* Processes MacClean may not inspect (for example, some owned by root) can show zero usage.
* The interval must be 1–3600 seconds. MacClean never terminates processes.

## Testing

```bash
sh scripts/check.sh
```

The script runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and a release build. It also builds every installed Apple target. The test suite (60 tests) uses only temporary directories with synthetic files. Destructive tests use a temporary directory as the home directory and check both what was deleted and what must remain. They cover:

* size calculations, directory aggregation, top-N limits, hard links
* permission failures, files disappearing mid-scan, symlink roots and loops
* dry-run planning never deleting anything, with exact estimates
* allowlist enforcement, protected and outside-home locations, path traversal, tampered plans
* symlinks inside candidates, symlinked candidates and roots, ancestors swapped after planning
* roots or candidates replaced between plan and execution, files changed or added after planning
* partial failures from unwritable directories, with accurate byte reporting
* Xcode running, roots owned by another user, missing categories
* CLI argument validation, exit codes, JSON validity, refusal without a TTY (including piped `yes`)
* the monitor's ranges, sorting and process visibility

### Measured on the development Mac (Apple M3, release build)

| Command | Wall time | Peak RSS |
|---|---|---|
| `analyze` on a synthetic 100,000-file tree | 0.6–1.4 s | 6.2 MB |
| `analyze ~/.npm` (34k files, 9.6k directories) | 1.2 s | 6.5 MB |
| `clean --dry-run` (real npm cache, ~5k files) | 0.22 s | 7.2 MB |
| `status` | < 0.01 s | 5.9 MB |
| `monitor --count 1` | 1.0 s (one interval) | 7.3 MB |

Memory stays flat as the tree grows, because the scanner keeps only the top-N results and the current directory path.

## Development notes

* The development shell named in the brief (Rush) was not installed in the build environment. All commands were run with zsh and Cargo, and `scripts/check.sh` is plain POSIX `sh`. MacClean does not need Rush at runtime.
* Dependencies: `clap` (CLI), `serde`/`serde_json` (JSON), `sysinfo` (CPU/memory/process metrics, `system` feature only), `thiserror` (errors), `libc` (`openat`/`fstatat`/`unlinkat`/`statfs`/`sysctl`), and `tempfile` (tests only).
* Modules: `cli`, `scanner`, `cleaner`, `policy`, `monitor`, `platform` (with `platform::fsops`), `report`, `errors`.
