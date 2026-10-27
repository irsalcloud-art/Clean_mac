#!/bin/sh
# Prints the GitHub Release notes for a version: its CHANGELOG.md section,
# the compatibility table from README.md (only combinations with a recorded
# test run), and how to download and verify the archives. POSIX sh.
#
#   sh scripts/release-notes.sh 0.2.0 > notes.md
#
# Fails when CHANGELOG.md has no "## [<version>]" section, so a release
# cannot go out without its changelog entry.
set -eu
cd "$(dirname "$0")/.."

version=${1:?usage: sh scripts/release-notes.sh <version>}
repo=https://github.com/irsalcloud-art/Clean_mac

section=$(awk -v v="$version" '
    index($0, "## [" v "]") == 1 { f = 1; next }
    f && /^## \[/ { exit }
    f && /^\[[^]]+\]: / { exit }
    f { print }
' CHANGELOG.md)
if test -z "$(printf '%s' "$section" | tr -d '[:space:]')"; then
    echo "release-notes: CHANGELOG.md has no '## [$version]' section" >&2
    exit 1
fi

compat=$(awk '
    /^## Compatibility/ { f = 1; next }
    f && /^## / { exit }
    f && /^\|/ { print }
    f && /^\*\*Supported minimum/ { print ""; print }
' README.md)

printf '%s\n' "$section" | sed -e '/./,$!d'
cat <<EOF

## Verified compatibility

Only these combinations have a recorded test run:

$compat

## Download and verify

Pick the archive for your Mac: \`aarch64-apple-darwin\` for Apple Silicon, \`x86_64-apple-darwin\` for Intel.

\`\`\`bash
curl -LO $repo/releases/download/v$version/macclean-$version-aarch64-apple-darwin.tar.gz
curl -LO $repo/releases/download/v$version/SHA256SUMS
shasum -a 256 -c SHA256SUMS --ignore-missing
tar -xzf macclean-$version-aarch64-apple-darwin.tar.gz
./macclean-$version-aarch64-apple-darwin/macclean --version
\`\`\`

The binaries are not code-signed or notarized yet ([#7]($repo/issues/7)). A file downloaded with a web browser is quarantined, and macOS will refuse to open it. Download with \`curl\` as above and check the checksum before you run it.
EOF
