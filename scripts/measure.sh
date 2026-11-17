#!/usr/bin/env bash
# Budget measurement on Unix: builds release, generates the fixture, runs scan, diff and tree
# three times each and prints the median for every row of spec section 6 this platform can
# measure. Linux numbers do not count toward the Windows gates; use scripts/measure.ps1 for those.
#
# usage: scripts/measure.sh [FILES] [DIRS] [DEPTH]      (defaults: 100000 12000 12)
set -euo pipefail

FILES="${1:-100000}"
DIRS="${2:-12000}"
DEPTH="${3:-12}"
SEED=1
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/arkos-measure.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
export ARKOS_HOME="$WORK/home"

cd "$ROOT"
cargo build --release --bin arkos --example gen_tree >&2
BIN="$ROOT/target/release/arkos"
GEN="$ROOT/target/release/examples/gen_tree"

now_ms() { echo $(( $(date +%s%N) / 1000000 )); }
median() { sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'; }

# Run a command, print "wall_ms peak_kib" on stdout, discard the command's own output.
run_timed() {
  local start end peak=0 pid
  start=$(now_ms)
  if [ -x /usr/bin/time ]; then
    /usr/bin/time -v -o "$WORK/time.txt" "$@" >/dev/null 2>&1
    peak=$(awk '/Maximum resident set size/ {print $NF}' "$WORK/time.txt")
  else
    "$@" >/dev/null 2>&1 &
    pid=$!
    while kill -0 "$pid" 2>/dev/null; do
      local hwm
      hwm=$(awk '/VmHWM/ {print $2}' "/proc/$pid/status" 2>/dev/null || true)
      [ -n "${hwm:-}" ] && [ "$hwm" -gt "$peak" ] && peak=$hwm
      sleep 0.02
    done
    wait "$pid"
  fi
  end=$(now_ms)
  echo "$((end - start)) $peak"
}

echo "== fixture: files=$FILES dirs=$DIRS depth=$DEPTH seed=$SEED"
MANIFEST=$("$GEN" --files "$FILES" --dirs "$DIRS" --depth "$DEPTH" --seed "$SEED" --out "$WORK/tree")
echo "$MANIFEST"
HASH=$(echo "$MANIFEST" | sed -n 's/.*hash=\([0-9a-f]*\).*/\1/p')

scan_ms=(); peak_kib=(); rollup_ms=()
for _ in 1 2 3; do
  rm -rf "$ARKOS_HOME"
  read -r wall peak < <(run_timed "$BIN" scan --volume "$WORK/tree" --mode walk)
  scan_ms+=("$wall"); peak_kib+=("$peak")
done
# One more scan, kept, for rollup_ms, the database size and the diff pair.
"$BIN" scan --volume "$WORK/tree" --mode walk --json > "$WORK/scan.json"
rollup_ms=$(sed -n 's/.*"rollup_ms": \([0-9]*\).*/\1/p' "$WORK/scan.json")
SNAP_A=$(sed -n 's/.*"snapshot_id": \([0-9]*\).*/\1/p' "$WORK/scan.json")

# Change about one file in a hundred, then take the second snapshot.
find "$WORK/tree" -type f | awk 'NR % 100 == 0' | head -n 2000 | xargs -r -d '\n' truncate -s 1M
"$BIN" scan --volume "$WORK/tree" --mode walk --json > "$WORK/scan2.json"
SNAP_B=$(sed -n 's/.*"snapshot_id": \([0-9]*\).*/\1/p' "$WORK/scan2.json")

diff_ms=(); tree_ms=()
for _ in 1 2 3; do
  read -r wall _ < <(run_timed "$BIN" diff "$SNAP_A" "$SNAP_B" --top 50 --json)
  diff_ms+=("$wall")
done
for i in 1 2 3; do
  read -r wall _ < <(run_timed "$BIN" tree --snapshot "$SNAP_B" --depth 12 --out "$WORK/tree-$i.html")
  tree_ms+=("$wall")
done
tree_bytes=$(stat -c %s "$WORK/tree-1.html")

list_ms=()
for _ in $(seq 10); do
  read -r wall _ < <(run_timed "$BIN" list)
  list_ms+=("$wall")
done

db_bytes=$("$BIN" db info --json | sed -n 's/.*"db_bytes": \([0-9]*\).*/\1/p')
wal_bytes=$("$BIN" db info --json | sed -n 's/.*"wal_bytes": \([0-9]*\).*/\1/p')
bin_bytes=$(stat -c %s "$BIN")

med_scan=$(printf '%s\n' "${scan_ms[@]}" | median)
med_peak=$(printf '%s\n' "${peak_kib[@]}" | median)
med_diff=$(printf '%s\n' "${diff_ms[@]}" | median)
med_tree=$(printf '%s\n' "${tree_ms[@]}" | median)
med_list=$(printf '%s\n' "${list_ms[@]}" | median)
rate=$(( FILES * 1000 / (med_scan > 0 ? med_scan : 1) ))

echo "== results (median of 3; list is median of 10)"
echo "platform            $(uname -srm)"
echo "fixture hash        $HASH"
echo "walk files/s        $rate   (gate >= 200000, fails < 150000)"
echo "scan wall ms        $med_scan"
echo "rollup ms           $rollup_ms   (gate < 3000 at 1M files)"
echo "peak working set    $((med_peak * 1024)) B   (gate <= 60000000)"
echo "db bytes/snapshot   $((db_bytes / 2))   (two snapshots in $db_bytes, wal $wal_bytes; gate <= 75000000 at 1M files)"
echo "diff ms             $med_diff   (gate < 2000 at 1M files)"
echo "tree ms             $med_tree   (gate < 4000 at 100k dirs)"
echo "tree output bytes   $tree_bytes   (gate < 40000000)"
echo "list cold ms        $med_list   (gate < 200)"
echo "binary bytes        $bin_bytes   (gate <= 8000000)"
echo "mft                 not measurable on this platform"
