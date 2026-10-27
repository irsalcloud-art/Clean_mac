#!/bin/sh
# Packages a release build of macclean for one target into dist/:
#   dist/macclean-<version>-<target>.tar.gz
#   dist/macclean-<version>-<target>.tar.gz.sha256
# POSIX sh, so it runs from Rush, zsh, bash or CI alike.
#
#   sh scripts/package.sh aarch64-apple-darwin
#   sh scripts/package.sh x86_64-apple-darwin [path/to/macclean]
#
# Build first (`cargo build --release --target <target>`). The script checks
# the binary before packaging it: architecture, minimum macOS version and,
# when this Mac can run it, `macclean --version`.
set -eu
cd "$(dirname "$0")/.."

target=${1:?usage: sh scripts/package.sh <target-triple> [binary]}
bin=${2:-target/$target/release/macclean}
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)

case $target in
    aarch64-apple-darwin) arch=arm64 ;;
    x86_64-apple-darwin) arch=x86_64 ;;
    *)
        echo "unsupported target: $target" >&2
        exit 2
        ;;
esac

fail() {
    echo "package: $*" >&2
    exit 1
}

test -n "$version" || fail "cannot read the version from Cargo.toml"
test -f "$bin" || fail "$bin does not exist; build it with: cargo build --release --target $target"

archs=$(lipo -archs "$bin")
test "$archs" = "$arch" || fail "$bin contains '$archs', expected '$arch'"

minos=$(otool -l "$bin" | awk '/LC_BUILD_VERSION/ {f=1} f && $1 == "minos" {print $2; exit}')
test "$minos" = "11.0" || fail "$bin has minos '$minos', expected 11.0"

# Run the binary when this Mac can (natively, or x86_64 under Rosetta 2).
if arch "-$arch" /usr/bin/true 2>/dev/null; then
    got=$(arch "-$arch" "$bin" --version)
    test "$got" = "macclean $version" || fail "$bin --version printed '$got', expected 'macclean $version'"
else
    echo "note: this Mac cannot run $arch code; skipped the --version check"
fi

name=macclean-$version-$target
mkdir -p dist
rm -rf "dist/$name" "dist/$name.tar.gz" "dist/$name.tar.gz.sha256"
mkdir "dist/$name"
cp "$bin" "dist/$name/macclean"
chmod 755 "dist/$name/macclean"
cp README.md CHANGELOG.md LICENSE-MIT LICENSE-APACHE "dist/$name/"

# No AppleDouble files or extended attributes, and no local user names.
(
    cd dist
    COPYFILE_DISABLE=1 tar --no-mac-metadata --uid 0 --gid 0 --uname root --gname wheel \
        -czf "$name.tar.gz" "$name"
    shasum -a 256 "$name.tar.gz" >"$name.tar.gz.sha256"
)
rm -rf "dist/$name"

echo "packaged dist/$name.tar.gz"
cat "dist/$name.tar.gz.sha256"
