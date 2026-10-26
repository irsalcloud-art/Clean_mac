# Security policy

MacClean deletes files. A bug that deletes something outside its allowlist, follows a symbolic link, or deletes without your confirmation counts as a security vulnerability, even if no attacker is involved.

## Supported versions

Only the latest release on `main` gets fixes. There are no backports while the project is at 0.x.

| Version | Supported |
|---|---|
| 0.1.x | Yes |

## Reporting a vulnerability

**Do not open a public issue for a vulnerability.** Report it privately through GitHub instead:

1. Open the repository's [Security tab](https://github.com/irsalcloud-art/Clean_mac/security).
2. Choose **Report a vulnerability** ([GitHub's guide](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)).

Include:

* the MacClean version (`macclean --version`), macOS version and architecture (`macclean status`)
* the exact command you ran
* the directory layout needed to reproduce it, ideally as a script that builds it inside a temporary directory
* what was deleted, or could be deleted, that should not have been

Do not send the contents of your real caches or home directory. A synthetic reproduction is enough.

## What to expect

This is a volunteer project with no guaranteed response time. The goals are:

* acknowledge the report within 7 days
* confirm or reject it, with reasoning, within 30 days
* fix confirmed issues that can delete data outside a cache before any other work, then publish a GitHub security advisory crediting the reporter (unless you ask to stay anonymous)

## In scope

* deleting, or planning to delete, anything outside the allowlisted cache entries described in the README
* following a symbolic link, or a race (TOCTOU) that redirects a deletion
* deleting without an interactive `yes` confirmation at a TTY
* reports that understate what was or would be deleted
* any network access, telemetry, or privilege escalation

## Out of scope

* actions by a process already running as your own user. It could delete the same files itself, and the README lists this as a known limitation.
* the size of the space reclaimed differing from the estimate (APFS clones, snapshots, open files)
* problems that only occur when MacClean runs as root or under `sudo`. MacClean never needs elevated privileges.

The design and its known limitations are described in the README's [Security model](README.md#security-model) and in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
