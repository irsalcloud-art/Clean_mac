# MacClean Roadmap

The roadmap follows one rule: **prove before you expand**. Each goal has to meet its exit criteria before work moves on to the next one. Compatibility and safety come before new features.

Every item is a GitHub issue with a priority label:

| Label | Meaning |
|---|---|
| `priority: P0` | Critical: blocks a trustworthy release |
| `priority: P1` | High: the next release |
| `priority: P2` | Medium: planned |
| `priority: P3` | Low: nice to have |

Goals map to [milestones](https://github.com/irsalcloud-art/Clean_mac/milestones). The pinned "Roadmap" issue tracks progress live.

## Where we are: v0.1.3 (released 2026-10-28)

[v0.1.3](https://github.com/irsalcloud-art/Clean_mac/releases/tag/v0.1.3) is the latest release. It hardens [v0.1.2](https://github.com/irsalcloud-art/Clean_mac/releases/tag/v0.1.2) from the same day after an external review, and adds no features. [v0.1.1](https://github.com/irsalcloud-art/Clean_mac/releases/tag/v0.1.1), on 2026-10-27, was the first with downloadable binaries. These releases ship work from several goals ahead of the version plan below. Each goal stays open until its exit criteria are met.

* `status`, `analyze` (with `--max-depth` and `--exclude`; cloud-synced folders skipped by default), `monitor` (with physical footprint and `--tui`), `report`
* `clean` categories: npm-cache, xcode-derived-data, pip-cache, homebrew-cache, gradle-cache, cocoapods-cache, yarn-cache. The Yarn 2+ global cache and the pnpm store are deliberately not cleaned, because projects use them directly.
* 127 tests, run in CI on an Apple Silicon and an Intel runner (macOS 15.7) and under Rosetta 2, plus by hand on an Apple M3 (macOS 27.0.1)
* Release archives for `aarch64`, `x86_64` and universal, with `SHA256SUMS`. Not yet code-signed or notarized ([#7](https://github.com/irsalcloud-art/Clean_mac/issues/7))

### Issue status

"Implemented" or "released" does not mean "verified". Each issue is tracked on four separate lines: the code exists, automated tests cover it, it was checked on a real Mac (not only on synthetic fixtures), and it ships in a release. An issue closes when its acceptance criteria are met; anything those criteria do not cover is listed as remaining work.

| Issue | Code | Automated tests | Checked on a real Mac | Released | Acceptance criteria | Remaining |
|---|---|---|---|---|---|---|
| #1 CI | Done | Both CI jobs on every push and PR | Intel runner is a VM on Intel Mac hardware | n/a | Met | none |
| #2 Intel and oldest macOS | n/a | Suite on `macos-15-intel` | Yes | v0.1.1 | Met (closed) | macOS 11–14 untested, so unsupported |
| #3 Refuse npm-cache while npm runs | Done | Yes | Refused with a live `npm exec` on the M3 | v0.1.1 | Met | none |
| #4 Property and fuzz tests | Done | Run in CI | n/a | v0.1.1 | Met | none |
| #5 Entries owned by another user | Done | Yes | Not recorded | v0.1.1 | Met | a real `sudo npm` cache has not been tried |
| #6 Release archives with checksums | Done | Release workflow | Downloaded and verified (v0.1.1–v0.1.3) | v0.1.1 | Met | none |
| #7 Code signing and notarization | Not started | none | none | no | Not met | needs an Apple Developer ID |
| #8 Universal binary | Done | Smoke test on both architectures in the release workflow | Both slices on the M3 | v0.1.1 | Met | none |
| #9 Custom DerivedData location | Done | Yes | Not recorded | v0.1.1 | Met | none |
| #10 Global and project npmrc | Done | Yes | Not recorded | v0.1.1 | Met | none |
| #11 Physical footprint | Done | Yes | Compared with `top` on the M3 | v0.1.2 | Met | none |
| #12 `--exclude` and `--max-depth` | Done | Yes | `analyze ~ --max-depth 2` on the M3 | v0.1.2 | Met | none |
| #13 New cleanup categories | Done | Yes, per category | Real caches: Gradle, Homebrew (dry run), npm. Synthetic only: pip, CocoaPods, Yarn 1 | v0.1.2 | Met | pip, CocoaPods and Yarn 1 on real caches |
| #14 Full-screen TUI | Done | Yes | Pseudo-terminal on the M3 | v0.1.2 | Met | none |
| #15 Homebrew tap | Done | `brew test` | Apple Silicon only | tap at v0.1.3 | Not met | install and uninstall on an Intel Mac |

* **Goal 1:** CI runs on every push and pull request on both architectures. Since 2026-10-28 a ruleset on `main` requires a pull request with both CI jobs passing, and blocks force-pushes and deletion; the maintainer bypasses it to push directly after running `scripts/check.sh` (see CONTRIBUTING.md). The exit criteria are met.
* **Goals 2 and 4:** every issue meets its acceptance criteria. Categories went in as separate commits pushed to `main` after the full suite passed, rather than as separate pull requests.
* **Goal 3:** blocked on #7. #15 also needs an Intel install.
* No security audit by a third party has been done. v0.1.3 addressed the findings of an external code review (executor approval, scanner symlink checks, pinned actions); see the [CHANGELOG](CHANGELOG.md).

```
v0.1.0 ──► Goal 1 ──► Goal 2 ──► Goal 3 ──► Goal 4
 done      Verified   Safer      Distri-    Coverage
           compat.    cleaner    bution     & UX
           v0.1.1     v0.2.0     v0.3.0     v0.4.0
```

## Goal 1: Verified compatibility (v0.1.1)

Prove MacClean runs correctly where the README says it runs.

| Priority | Issue |
|---|---|
| P0 | [#1 CI: fmt, clippy and tests on Apple Silicon and Intel runners](https://github.com/irsalcloud-art/Clean_mac/issues/1) |
| P0 | [#2 Validate on real Intel hardware and on the oldest supported macOS](https://github.com/irsalcloud-art/Clean_mac/issues/2) |

**Exit criteria:** CI is required on every pull request. The test suite has passed on a native Intel Mac. The README compatibility table lists only tested combinations.

## Goal 2: Safer cleaner (v0.2.0)

Close the known gaps in the safety model before adding more cleanup categories.

| Priority | Issue |
|---|---|
| P1 | [#3 Refuse npm-cache cleanup while npm/npx/yarn/pnpm is running](https://github.com/irsalcloud-art/Clean_mac/issues/3) |
| P1 | [#4 Property and fuzz tests for the policy and deletion](https://github.com/irsalcloud-art/Clean_mac/issues/4) |
| P1 | [#5 Guidance when cache entries are owned by another user](https://github.com/irsalcloud-art/Clean_mac/issues/5) |
| P2 | [#9 Custom Xcode DerivedData location](https://github.com/irsalcloud-art/Clean_mac/issues/9) |
| P2 | [#10 Global and project-level npmrc](https://github.com/irsalcloud-art/Clean_mac/issues/10) |

**Exit criteria:** every limitation listed in the README's security section is fixed or explicitly accepted. The stress tests in CI show zero writes outside the fixture root.

## Goal 3: Distribution (v0.3.0)

Users can install a verified binary without a Rust toolchain.

| Priority | Issue |
|---|---|
| P1 | [#6 Release artifacts with checksums for arm64 and x86_64](https://github.com/irsalcloud-art/Clean_mac/issues/6) |
| P2 | [#7 Code-sign and notarize release binaries](https://github.com/irsalcloud-art/Clean_mac/issues/7) |
| P2 | [#8 Universal binary](https://github.com/irsalcloud-art/Clean_mac/issues/8) (needs Goal 1) |
| P3 | [#15 Homebrew tap](https://github.com/irsalcloud-art/Clean_mac/issues/15) (needs #6, #7) |

**Exit criteria:** a downloaded release passes Gatekeeper and its checksum verification, and installs and uninstalls cleanly on both architectures.

## Goal 4: More coverage and UX (v0.4.0)

Add value carefully. Each new cleanup category ships in its own pull request, with its own policy and destructive tests.

| Priority | Issue |
|---|---|
| P2 | [#11 Physical footprint memory in monitor](https://github.com/irsalcloud-art/Clean_mac/issues/11) |
| P2 | [#12 analyze: `--exclude` and `--max-depth`](https://github.com/irsalcloud-art/Clean_mac/issues/12) |
| P3 | [#13 New cleanup categories (pip, Homebrew, Yarn/pnpm, Gradle, CocoaPods)](https://github.com/irsalcloud-art/Clean_mac/issues/13) (needs Goal 2) |
| P3 | [#14 Live full-screen TUI for monitor](https://github.com/irsalcloud-art/Clean_mac/issues/14) |

**Exit criteria:** every new category refuses to run while its tool is active, classifies unrecognized entries as "review", and has tests showing which files survive.

## Dependencies

* #8 (universal binary) needs #1 and #2.
* #13 (new categories) needs #3 and #4, so new categories inherit the stronger safety checks.
* #15 (Homebrew tap) needs #6 and #7.
* Every other issue can start as soon as its goal is reached.

## Out of scope

These are deliberately not planned, to keep MacClean trustworthy:

* deleting arbitrary user-chosen paths
* "RAM cleaning", killing processes, or changing system settings
* privileged helpers, daemons, `sudo`, telemetry, or network access
* claims that MacClean makes a Mac faster

## Updating this roadmap

When an issue closes, tick it in the pinned Roadmap issue. When a goal's exit criteria are met, close its milestone and update "Where we are" above. New work starts as an issue with a priority label and milestone, then gets added here.
