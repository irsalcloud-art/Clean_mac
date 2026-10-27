# Releasing

Releases are built by `.github/workflows/release.yml`. Pushing a tag `v<version>` that matches `Cargo.toml` makes the workflow:

1. On a native Apple Silicon runner (`macos-15`) and a native Intel runner (`macos-15-intel`): run `scripts/check.sh` (fmt, clippy, the full test suite, release builds), smoke-test the release binary, and package it with `scripts/package.sh`. The script checks the binary's architecture, `minos 11.0` and `macclean --version` before it writes the archive and its `.sha256`.
2. Combine the two tested binaries with `lipo -create` into a universal binary, smoke-test it as arm64 and as x86_64 (Rosetta 2) on `macos-15`, package it, and smoke-test the packaged archive natively on `macos-15-intel`.
3. Verify the three checksums, merge them into `SHA256SUMS`, write the notes with `scripts/release-notes.sh`, and create the GitHub Release with the three archives and `SHA256SUMS`.

The release fails if the tag and `Cargo.toml` disagree, if `CHANGELOG.md` has no section for the version, or if any test fails on either architecture.

Running the workflow by hand (Actions → Release → Run workflow) does steps 1 and 2 only, as a dry run, and keeps the archives as workflow artifacts.

## Steps

1. Update the version in `Cargo.toml`, then run `cargo build` so `Cargo.lock` follows.
2. In `CHANGELOG.md`, rename `## [Unreleased]` to `## [<version>] - <YYYY-MM-DD>`, add a new empty `## [Unreleased]` above it, and update the compare links at the bottom.
3. Check that the README compatibility table lists only combinations with a recorded test run. It is copied into the release notes.
4. Preview the release locally:

   ```bash
   sh scripts/check.sh
   ```

   ```bash
   sh scripts/package.sh aarch64-apple-darwin
   ```

   ```bash
   sh scripts/package.sh x86_64-apple-darwin
   ```

   ```bash
   sh scripts/release-notes.sh <version>
   ```

5. Commit as `chore(release): <version>`, get it onto `main`, and wait for CI to pass.
6. Tag that commit and push the tag:

   ```bash
   git tag -a v<version> -m "MacClean <version>"
   ```

   ```bash
   git push origin v<version>
   ```

7. When the workflow finishes, download an archive with `curl`, run `shasum -a 256 -c SHA256SUMS --ignore-missing`, and run `macclean --version`.

Binaries are not code-signed or notarized yet. That is [#7](https://github.com/irsalcloud-art/Clean_mac/issues/7).
