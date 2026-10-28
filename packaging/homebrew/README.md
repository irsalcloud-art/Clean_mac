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

On the development Mac (Apple M3, macOS 27.0.1, Homebrew 7.0.10), with the v0.1.2 formula in a temporary local tap that was removed afterwards:

* `brew style`: no offenses. An earlier version with separate `url`s inside `on_arm`/`on_intel` failed it, which is why the formula uses the universal archive.
* `brew info`: version `0.1.2`, license `MIT OR Apache-2.0`.
* `brew fetch`: the download matched the pinned SHA-256.

`brew install` and `brew audit` did **not** run. Homebrew refused both because the Mac's Command Line Tools (26.6) are older than macOS 27 needs ("Your Command Line Tools are too outdated"). Homebrew requires current Command Line Tools for any formula without a bottle, even one that only copies a binary, so users with outdated tools will see the same message until they update them. Install, `brew test` and uninstall still need a run on an up-to-date Mac of each architecture. The universal binary itself was smoke-tested natively on Apple Silicon and Intel in the release workflow.

Homebrew downloads with `curl` and does not quarantine formula files, so the binary runs although it is not yet signed or notarized ([#7](https://github.com/irsalcloud-art/Clean_mac/issues/7)).
