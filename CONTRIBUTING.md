# Contributing to MacClean

Thanks for helping. MacClean deletes files on people's Macs, so correctness and safety come before features. This guide explains how to work on it and what a pull request needs before it can be merged.

By taking part you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md). For security problems, follow [SECURITY.md](SECURITY.md) instead of opening an issue.

## Ways to help

* **Compatibility reports.** Only macOS 15 and 27 have a recorded test run (see the README's [Compatibility](README.md#compatibility) table). Running the tests and the smoke test on macOS 11–14, or on a physical Intel Mac, and posting the output with the "Compatibility report" issue form is very useful.
* **Bug reports** with exact commands and output.
* **Issues labelled** [`good first issue`](https://github.com/irsalcloud-art/Clean_mac/labels/good%20first%20issue) or [`help wanted`](https://github.com/irsalcloud-art/Clean_mac/labels/help%20wanted).
* **Roadmap work.** [ROADMAP.md](ROADMAP.md) and the pinned Roadmap issue list what is planned, in priority order. Comment on an issue before you start on it, so two people don't do the same work.

## Development setup

You need macOS and a stable Rust toolchain. Edition 2024 needs at least Rust 1.85, but only 1.95 and the latest stable used in CI have been tested. MacClean does not compile on other operating systems.

```bash
git clone https://github.com/irsalcloud-art/Clean_mac.git
```

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
```

The second target is optional, but `scripts/check.sh` builds every Apple target you have installed. On Apple Silicon with Rosetta 2, you can also run the x86_64 tests:

```bash
cargo test --target x86_64-apple-darwin
```

## Before you open a pull request

Run the same checks as CI from the repository root:

```bash
sh scripts/check.sh
```

It runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and release builds. Then smoke-test the binary. It is read-only and deletes nothing:

```bash
sh scripts/smoke.sh target/release/macclean
```

CI runs both on an Apple Silicon runner and on an Intel runner, and also runs the x86_64 tests under Rosetta 2. Both jobs must pass. `main` is protected: contributors' changes go through a pull request, and it cannot be merged until both jobs are green. The maintainer may push directly to `main` after running `scripts/check.sh`; CI then runs on the pushed commit, and a red run is fixed before anything else lands. If GitHub cancels a job (it sometimes cancels the Apple Silicon runner for lack of capacity), re-run it.

## Pull request guidelines

* **One change per pull request.** Keep refactors separate from behavior changes.
* **Commit messages** follow [Conventional Commits](https://www.conventionalcommits.org/): `type(scope): description`, for example `fix(cleaner): skip entries with immutable flags`. Common types: `feat`, `fix`, `docs`, `test`, `refactor`, `ci`, `chore`. Scopes are module or area names: `cleaner`, `scanner`, `policy`, `monitor`, `cli`, `report`, `platform`, `compat`, `roadmap`.
* **Reference the issue**, for example `Closes #12`, in the pull request description.
* **Tests are required** for behavior changes. Bug fixes need a test that fails without the fix.
* **Update the docs in the same pull request:** the README for user-visible behavior, and [CHANGELOG.md](CHANGELOG.md) under "Unreleased".
* **Do not claim untested support.** The README only lists hardware and macOS versions with a recorded run.
* **No new dependencies** without discussing them in an issue first. The current set is deliberately small.

## Rules for code that deletes files

These are not negotiable. A pull request that breaks one will not be merged, however useful it is otherwise.

1. **Planning and scanning only read.** Deletion happens only in `cleaner::execute`, and only after the CLI has a typed `yes` from a TTY.
2. **Every destructive syscall goes through `platform::fsops`.** These functions work relative to an open directory descriptor and use `O_NOFOLLOW`. Don't call `std::fs::remove_*` in non-test code.
3. **Compare paths by component**, using `Path::starts_with` or `policy::is_within`, and never by string prefix.
4. **Never follow symbolic links.** A symlink inside a cache is unlinked as a link entry, and its target is never touched.
5. **Unknown means keep.** An entry that isn't positively recognized is classified as `Review` and is never deleted.
6. **Revalidate before deleting.** Anything that changed between plan and execution is skipped and reported.
7. **Report exactly.** Byte and file counts include only successful `unlink`s, and every skip or failure is listed.
8. **Tests use temporary directories only.** Use `tempfile` fixtures that stand in for the home directory, as in `tests/cleaner.rs`. A test must never read or write the real home directory or real caches.

Adding a cleanup category touches all of these rules. Read [docs/ADDING_A_CATEGORY.md](docs/ADDING_A_CATEGORY.md) and open an issue to agree on the category before writing code.

## Project layout

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how the modules fit together.

## License

MacClean is dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in the work, as defined in the Apache-2.0 license, is dual-licensed as above, without any additional terms or conditions.
