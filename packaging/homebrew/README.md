# Homebrew formula

`macclean.rb` is a formula for a Homebrew tap. It installs the universal (arm64 + x86_64) release archive, pinned to its SHA-256, and puts `macclean` on the `PATH`. The tap repository does not exist yet ([#15](https://github.com/irsalcloud-art/Clean_mac/issues/15)).

## Publishing the tap (repository owner)

1. Create a public repository named `homebrew-macclean` under `irsalcloud-art`. Homebrew maps `irsalcloud-art/macclean` to that name.
2. Add this file as `Formula/macclean.rb` and push it.

Users can then install and remove MacClean with:

```bash
brew install irsalcloud-art/macclean/macclean
```

```bash
brew uninstall macclean
```

```bash
brew untap irsalcloud-art/macclean
```

## Updating for a new release

After a release is published, take the line for `macclean-<version>-universal-apple-darwin.tar.gz` from its `SHA256SUMS`, then update `url` and `sha256` in the formula. Homebrew reads the version from the URL. Check it with:

```bash
brew style irsalcloud-art/macclean/macclean
```

```bash
brew fetch irsalcloud-art/macclean/macclean
```

## What has been tested

On the development Mac (Apple M3, macOS 27.0.1, Homebrew 7.0.10, Command Line Tools 27.0), with the v0.1.2 formula in a temporary local tap that was removed afterwards:

* `brew style`: no offenses. An earlier version with separate `url`s inside `on_arm`/`on_intel` failed it, which is why the formula uses the universal archive.
* `brew info`: version `0.1.2`, license `MIT OR Apache-2.0`.
* `brew fetch`: the download matched the pinned SHA-256.
* `brew install`: installed to `/opt/homebrew/Cellar/macclean/0.1.2` and linked `macclean` into `/opt/homebrew/bin`. The installed file is the universal (x86_64 + arm64) binary.
* `brew test`: passed (`--version`, `status --format json`, and `clean --dry-run --category npm-cache`).
* `brew audit --strict` and `brew audit --new --online`: no problems.
* The installed `macclean --version`, `macclean status` and `macclean clean --dry-run` ran normally. The binary had no `com.apple.quarantine` attribute.
* `brew uninstall` and `brew untap` removed the binary, the link and the tap. Afterwards the Mac had no taps and no `macclean` on the `PATH`, and the same 105 formulae were installed as before. Homebrew keeps the downloaded archive in `~/Library/Caches/Homebrew` after uninstalling; `brew cleanup` removes it.

Homebrew needs current Command Line Tools to install any formula that has no bottle, even one that only copies a binary. With CLT 26.6 on macOS 27, `brew install` and `brew audit` stopped with "Your Command Line Tools are too outdated", and users with outdated tools will see the same message until they update them. This was tested only on Apple Silicon; an install on an Intel Mac has not been run. The universal binary itself was smoke-tested natively on Apple Silicon and Intel in the release workflow.

Homebrew downloads with `curl` and does not quarantine formula files, so the binary runs although it is not yet signed or notarized ([#7](https://github.com/irsalcloud-art/Clean_mac/issues/7)).
