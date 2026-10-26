#!/bin/sh
# Smoke test of a built macclean binary on the current machine: prints the
# platform facts MacClean reads (sysctl, statfs) and runs every read-only
# command in text and JSON form. Never deletes anything.
#
#   sh scripts/smoke.sh [path/to/macclean]
#
# Defaults to target/release/macclean. Fails on the first non-zero exit.
set -eu
cd "$(dirname "$0")/.."
bin=${1:-target/release/macclean}
test -x "$bin" || { echo "no executable at $bin (run cargo build --release)" >&2; exit 1; }

run() {
    echo
    echo "\$ macclean $*"
    "$bin" "$@"
}

echo "== platform"
sw_vers
echo "uname -m: $(uname -m)"
file "$bin"
for key in kern.osproductversion vm.pagesize hw.optional.arm64 hw.memsize; do
    echo "$key: $(sysctl -n "$key" 2>/dev/null || echo '(missing)')"
done
df -k /

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
