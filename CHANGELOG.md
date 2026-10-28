# Changelog

All notable changes to MacClean are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html). While the version is 0.x, minor releases may change behavior.

## [Unreleased]

## [0.1.4] - 2026-10-28

Security fix: `clean` could print environment variables, including tokens, when it refused a category because a tool was running. Affects 0.1.2 and 0.1.3; update if you use either.

### Security
- A "refused while … is running" message could print environment variables, including any tokens in them. npm rewrites its process title, and the argument list macOS then reports for it runs on into the environment; `clean` showed that list in text and JSON output. Command lines in these messages now leave out every `NAME=value` entry. Introduced in 0.1.2; 0.1.1 showed only the process title.

## [0.1.3] - 2026-10-28

Hardening after an external review: deletion now needs an approved plan at the API level, `analyze` no longer enters a folder swapped for a symlink mid-scan, and CI actions are pinned to commit SHAs. No new cleanup categories.

### Added
- Dependabot keeps the pinned GitHub Actions current.
- MacClean can be installed with Homebrew: `brew install irsalcloud-art/macclean/macclean` installs the universal binary from the new [irsalcloud-art/homebrew-macclean](https://github.com/irsalcloud-art/homebrew-macclean) tap, pinned to its release checksum. Tested on Apple Silicon so far ([#15](https://github.com/irsalcloud-art/Clean_mac/issues/15)).

### Changed
- `cleaner::execute` takes an `ApprovedPlan`, which only `CleanPlan::approve` makes from the exact answer `yes`, so the confirmation is enforced by the deletion API and not only by the CLI.
- `analyze` reports each path with its parent folders resolved, so a path reached through a symlinked folder shows where the scan really happened.
- `clean` drops repeated `--category` values even when they are not next to each other.
- The Homebrew formula requires macOS 15 (Sequoia), the documented minimum.
- GitHub Actions in CI and release workflows are pinned to full commit SHAs.

### Fixed
- `analyze` could enter a different folder than the one reported if a folder in the path was swapped for a symlink while it ran. Each folder is now opened without following symlinks and checked against what was examined and against its parent before it is read.

## [0.1.2] - 2026-10-28

New cleanup categories for pip, Homebrew, Gradle, CocoaPods and Yarn 1, a full-screen `monitor --tui`, physical footprint in `monitor`, and `analyze --max-depth`/`--exclude`. `analyze` no longer enters cloud-synced folders by default.

### Added
- `clean --category yarn-cache` removes Yarn 1's cache folders (`v6`, older `v<N>`) in `~/Library/Caches/Yarn`, like `yarn cache clean`. It is refused while npm, npx, yarn or pnpm is running or when there is no `v<N>` folder, and reports `YARN_CACHE_FOLDER`, `XDG_CACHE_HOME` and `cache-folder` settings. The Yarn 2+ global cache and the pnpm store are deliberately not supported, because installed projects use them directly ([#13](https://github.com/irsalcloud-art/Clean_mac/issues/13)).
- `clean --category cocoapods-cache` removes the `Pods` folder in `~/Library/Caches/CocoaPods`, exactly what `pod cache clean --all` removes. It is refused while `pod` is running or when there is no `Pods` folder, keeps `search_index.json` and unknown entries, and reports `CP_CACHE_DIR`, `CP_HOME_DIR` and `cache_root` settings without following them ([#13](https://github.com/irsalcloud-art/Clean_mac/issues/13)).
- `monitor --tui`: a full-screen live view that updates in place, fits the terminal, and changes ranking with `c`, `m` and `f`. It has no new dependency, restores the terminal on every exit, and costs no measurable CPU over `--count 0`. The text and NDJSON modes are unchanged ([#14](https://github.com/irsalcloud-art/Clean_mac/issues/14)).
- `clean --category gradle-cache` removes Gradle's version-specific caches and its `modules-2`, `jars-N`, `transforms-N` and `build-cache-N` caches from `~/.gradle/caches`. It is refused while any Gradle daemon, build or wrapper is running (the message suggests `gradle --stop`) or when the folder does not look like Gradle's, and keeps `journal-1`, `CACHEDIR.TAG`, unknown entries and everything outside `caches` ([#13](https://github.com/irsalcloud-art/Clean_mac/issues/13)).
- `clean --category homebrew-cache` removes Homebrew's `downloads` folder in `~/Library/Caches/Homebrew`, as `brew cleanup --prune=all` does for downloads. The links into it are left for Homebrew to prune. It is refused while brew is running or when there is no `downloads` folder, keeps `api`, `bootsnap`, `Cask` and unknown entries, and reports `HOMEBREW_CACHE` settings without following them ([#13](https://github.com/irsalcloud-art/Clean_mac/issues/13)).
- `clean --category pip-cache` removes `http`, `http-v2` and `wheels` from pip's default cache, `~/Library/Caches/pip`, as `pip cache purge` does. It is refused while pip is running or when the folder does not look like a pip cache, keeps `selfcheck` and unknown entries, and reports other `cache-dir` settings without following them ([#13](https://github.com/irsalcloud-art/Clean_mac/issues/13)).
- `analyze --max-depth N` ranks only directories up to N levels below each path, like `du -d`, while sizes still include everything below. `analyze --exclude GLOB` (repeatable) leaves out matching names or relative paths without entering or counting them, for example `--exclude node_modules` or `--exclude Library/CloudStorage` ([#12](https://github.com/irsalcloud-art/Clean_mac/issues/12)).
- `monitor` shows each process's physical footprint (Activity Monitor's "Memory" column) next to RSS, and `--sort footprint` ranks by it. Footprint is reported as unavailable for other users' processes ([#11](https://github.com/irsalcloud-art/Clean_mac/issues/11)).

### Changed
- `analyze` (and `report --path`) no longer enters cloud-synced folders, `~/Library/CloudStorage` and iCloud Drive's `~/Library/Mobile Documents`, because listing them waits on the sync app and can make it fetch data. The output lists the skipped folders; `--include-cloud` scans them, and a cloud folder passed as a path is always scanned. JSON adds `cloud_folders_skipped`.

## [0.1.1] - 2026-10-27

First release with downloadable binaries for Apple Silicon, Intel and both (universal), with SHA-256 checksums.

### Added
- GitHub Actions CI on an Apple Silicon (`macos-15`) and an Intel (`macos-15-intel`) runner: formatting, clippy, tests, and release builds for both architectures. It also checks `minos 11.0` and runs the x86_64 tests under Rosetta 2 ([#1](https://github.com/irsalcloud-art/Clean_mac/issues/1), [#17](https://github.com/irsalcloud-art/Clean_mac/pull/17)).
- `scripts/smoke.sh`: a read-only smoke test of every command, run in CI on both architectures ([#2](https://github.com/irsalcloud-art/Clean_mac/issues/2), [#18](https://github.com/irsalcloud-art/Clean_mac/pull/18)).
- Property tests for the root policy and entry-name checks, and a stress test that mutates the cache while cleanup runs, all bounded so they run in CI on every pull request ([#4](https://github.com/irsalcloud-art/Clean_mac/issues/4)).
- Release workflow: a `v<version>` tag builds, tests and smoke-tests each architecture on a native runner and publishes `macclean-<version>-aarch64-apple-darwin.tar.gz`, `macclean-<version>-x86_64-apple-darwin.tar.gz` and `SHA256SUMS`, with notes from this changelog and the tested-compatibility table. `scripts/package.sh` and `scripts/release-notes.sh` do the same locally ([#6](https://github.com/irsalcloud-art/Clean_mac/issues/6)).
- A universal (`arm64` + `x86_64`) archive in each release, built with `lipo` from the two tested binaries and smoke-tested natively on Apple Silicon, under Rosetta 2 and natively on Intel. `scripts/smoke.sh` takes `SMOKE_ARCH` to run a universal binary as a given architecture ([#8](https://github.com/irsalcloud-art/Clean_mac/issues/8)).
- Open-source project files: dual MIT/Apache-2.0 license, contributing guide, code of conduct, security policy, issue and pull request templates, and architecture docs.

### Changed
- `xcode-derived-data` follows a custom DerivedData location set in Xcode (`IDECustomDerivedDataLocation`, read without changing it) instead of always refusing it. The location gets the full root policy. Per-workspace (relative) locations, locations outside home, in protected folders, behind symlinks or with `..` are refused with a reason ([#9](https://github.com/irsalcloud-art/Clean_mac/issues/9)).
- npm cache detection now also reads the global npmrc (`globalconfig`, or `<prefix>/etc/npmrc` with the prefix found the way npm finds it, including Homebrew's) and npm's builtin npmrc, in npm's documented order. A project `.npmrc` that sets `cache=` is reported as a note on the plan but not used ([#10](https://github.com/irsalcloud-art/Clean_mac/issues/10)).
- `clean` groups entries owned by another user (often left by `sudo npm`) into one line with a count and a `sudo chown` command to run yourself, instead of listing every path. JSON output adds `foreign_owned` to each tally and `root` to each category result ([#5](https://github.com/irsalcloud-art/Clean_mac/issues/5)).
- `clean` refuses the `npm-cache` category while npm, npx, yarn or pnpm is running, both when planning and again just before deleting, so an active `npm install` is not broken ([#3](https://github.com/irsalcloud-art/Clean_mac/issues/3)).
- The supported minimum is now documented as macOS 15. Binaries still target macOS 11.0, but macOS 11–14 have no recorded test run ([#2](https://github.com/irsalcloud-art/Clean_mac/issues/2)).
- The README compatibility table lists only hardware and macOS versions with a recorded test run.

## [0.1.0] - 2026-10-26

First version.

### Added
- `status`: macOS version, architecture (including Rosetta 2 detection), memory, disk and uptime.
- `analyze`: the largest files and directories under the given paths, by allocated or logical size. It is read-only, stays on one filesystem by default and never follows symbolic links.
- `clean`: a dry-run cleanup plan for `npm-cache` and `xcode-derived-data`. With `--interactive`, it deletes only after you type `yes` at a terminal, after revalidating every candidate, without following symlinks.
- `monitor`: CPU, memory, disk and top processes, in text or NDJSON.
- `report`: status, cleanup plan and an optional analysis in one report.
- Text and JSON output for every command, and documented exit codes 0–5.

[Unreleased]: https://github.com/irsalcloud-art/Clean_mac/compare/v0.1.4...HEAD
[0.1.4]: https://github.com/irsalcloud-art/Clean_mac/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/irsalcloud-art/Clean_mac/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/irsalcloud-art/Clean_mac/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/irsalcloud-art/Clean_mac/compare/e7a562c...v0.1.1
[0.1.0]: https://github.com/irsalcloud-art/Clean_mac/commit/e7a562c
