#!/bin/sh
# Smoke test of a built macclean binary on the current machine: prints the
# platform facts MacClean reads (sysctl, statfs) and runs every read-only
# command in text and JSON form. Never deletes anything.
#
#   sh scripts/smoke.sh [path/to/macclean]
#   SMOKE_ARCH=x86_64 sh scripts/smoke.sh path/to/universal/macclean
#
# Defaults to target/release/macclean. Fails on the first non-zero exit.
# SMOKE_ARCH (arm64 or x86_64) runs every command as that architecture with
# arch(1), for universal binaries, and checks that this slice really ran.
set -eu
cd "$(dirname "$0")/.."
bin=${1:-target/release/macclean}
test -x "$bin" || { echo "no executable at $bin (run cargo build --release)" >&2; exit 1; }

smoke_arch=${SMOKE_ARCH:-}
case $smoke_arch in
    "") ;;
    arm64) expect_arch=aarch64 ;;
    x86_64) expect_arch=x86_64 ;;
    *) echo "SMOKE_ARCH must be arm64 or x86_64" >&2; exit 2 ;;
esac

mc() {
    if test -n "$smoke_arch"; then
        arch "-$smoke_arch" "$bin" "$@"
    else
        "$bin" "$@"
    fi
}

run() {
    echo
    echo "\$ macclean $*"
    mc "$@"
}

echo "== platform"
sw_vers
echo "uname -m: $(uname -m)"
file "$bin"
for key in kern.osproductversion vm.pagesize hw.optional.arm64 hw.memsize; do
    echo "$key: $(sysctl -n "$key" 2>/dev/null || echo '(missing)')"
done
df -k /

if test -n "$smoke_arch"; then
    echo "running as: $smoke_arch"
    mc status --format json | grep -q "\"binary_arch\": \"$expect_arch\"" \
        || { echo "the $smoke_arch slice did not run" >&2; exit 1; }
fi

echo
echo "== commands"
run --version
run status
run status --format json
run analyze --top 5 src tests
run analyze --top 5 --format json src
run clean --dry-run
run clean --dry-run --format json
run monitor --count 2 --interval 1 --top 5
run monitor --count 1 --top 5 --format json
run report --path src --top 5
echo
echo "smoke test passed"
