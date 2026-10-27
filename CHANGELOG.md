# Changelog

All notable changes to MacClean are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html). While the version is 0.x, minor releases may change behavior.

## [Unreleased]

### Added
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

[Unreleased]: https://github.com/irsalcloud-art/Clean_mac/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/irsalcloud-art/Clean_mac/compare/e7a562c...v0.1.1
[0.1.0]: https://github.com/irsalcloud-art/Clean_mac/commit/e7a562c
