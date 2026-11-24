#!/usr/bin/env bash
# Measure Sapu.app and SapuShell.app on this Mac and write a Markdown report.
#
# Usage: ./scripts/perf-macos.sh [report.md]
#   Needs dist/Sapu.app and dist/SapuShell.app (scripts/bundle-macos.sh,
#   macos/build-shell.sh) and a logged-in GUI session.
#   FILES=120000 changes the size of the test tree.
#
# What it measures, on a generated folder tree so runs are comparable:
#   1. the engine alone: `sapu scan` (wall time, peak memory) and the calls the
#      SwiftUI shell makes through sapu-ffi (scan_bench example);
#   2. each app: launch → scan done (from its log), peak and settled memory,
#      and idle CPU once the scan is over;
#   3. SapuShell with and without Liquid Glass (SAPU_NO_GLASS=1);
#   4. app and binary sizes.
# Each app runs with its own empty HOME, so nothing on this Mac is read or changed.
set -euo pipefail
cd "$(dirname "$0")/.."

REPORT=${1:-perf-report.md}
FILES=${FILES:-120000}
WORK=$(mktemp -d)
TREE="$WORK/tree"
trap 'rm -rf "$WORK"' EXIT

echo "▸ building the test tree (~$FILES files)"
python3 - "$TREE" "$FILES" <<'PY'
import os, sys
root, total = sys.argv[1], int(sys.argv[2])
made = 0
def write(path, n):
    global made
    with open(path, "wb") as f:
        f.write(b"x" * n)
    made += 1
# Projects with dependencies (half the files), documents, and some big files.
projects = max(1, total // 2 // 2000)
for p in range(projects):
    proj = os.path.join(root, "Projects", f"app{p}")
    os.makedirs(proj, exist_ok=True)
    write(os.path.join(proj, "package.json"), 200)
    for k in range(50):
        d = os.path.join(proj, "node_modules", f"pkg{k}")
        os.makedirs(d, exist_ok=True)
        for i in range(40):
            write(os.path.join(d, f"f{i}.js"), 1024 + i * 17)
i = 0
while made < total - 20:
    d = os.path.join(root, "Documents", f"folder{i // 200}")
    os.makedirs(d, exist_ok=True)
    write(os.path.join(d, f"note{i}.txt"), 512 + (i % 7) * 300)
    i += 1
big = os.path.join(root, "Downloads")
os.makedirs(big, exist_ok=True)
for j in range(20):
    write(os.path.join(big, f"installer{j}.dmg"), 8 * 1024 * 1024)
print(made, "files")
PY

echo "▸ building the engine binaries"
cargo build --quiet --release -p sapu-cli
cargo build --quiet --release -p sapu-ffi --example scan_bench

ms() { python3 -c 'import time; print(int(time.time()*1000))'; }

# ── 1. Engine ───────────────────────────────────────────────────────
echo "▸ engine: sapu scan ×3"
CLI_ROWS=""
for run in 1 2 3; do
  /usr/bin/time -l sh -c "target/release/sapu -q scan '$TREE' -t 1 >/dev/null 2>&1" 2> "$WORK/time.txt"
  real=$(awk '/ real/ {print $1}' "$WORK/time.txt")
  rss=$(awk '/maximum resident set size/ {printf "%.0f", $1/1048576}' "$WORK/time.txt")
  CLI_ROWS+="| $run | ${real} s | ${rss} MB |"$'\n'
done

echo "▸ engine: sapu-ffi scan_bench ×3"
FFI_JSON=$(target/release/examples/scan_bench "$TREE" 3)

# ── 2. Apps ─────────────────────────────────────────────────────────
# measure <name> <binary> <log file name> [env...]
# Prints: launch→scan ms | peak MB | settled MB | idle CPU %
measure() {
  local name=$1 bin=$2 logname=$3
  shift 3
  local home="$WORK/home-$name"
  mkdir -p "$home"
  local log="$home/Library/Logs/sapu/$logname"
  local start pid scan_ms="n/a" peak=0 rss cpu_sum=0 cpu_n=0 settled=0
  start=$(ms)
  env HOME="$home" "$@" "$bin" "$TREE" >/dev/null 2>&1 &
  pid=$!
  # Wait up to 120 s for "scan done" in the app's own log.
  for _ in $(seq 1 1200); do
    if ! kill -0 "$pid" 2>/dev/null; then break; fi
    rss=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ' || true)
    [ "${rss:-0}" -gt "$peak" ] && peak=$rss
    if [ -f "$log" ] && grep -q "scan done" "$log"; then
      scan_ms=$(( $(ms) - start ))
      break
    fi
    sleep 0.1
  done
  if kill -0 "$pid" 2>/dev/null; then
    sleep 3 # let the window settle
    for _ in $(seq 1 20); do
      rss=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ' || true)
      cpu=$(ps -o %cpu= -p "$pid" 2>/dev/null | tr -d ' ' || true)
      [ -n "$rss" ] && [ -n "$cpu" ] || break
      [ "$rss" -gt "$peak" ] && peak=$rss
      settled=$rss
      cpu_sum=$(python3 -c "print($cpu_sum + $cpu)")
      cpu_n=$((cpu_n + 1))
      sleep 0.5
    done
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    printf '%s | %s MB | %s MB | %s %%' "$scan_ms" "$((peak / 1024))" "$((settled / 1024))" \
      "$(python3 -c "print(round($cpu_sum / max($cpu_n, 1), 1))")"
  else
    printf 'did not start | | |'
  fi
}

echo "▸ app: Sapu.app"
SAPU=$(measure sapu dist/Sapu.app/Contents/MacOS/Sapu sapu-gui.log)
echo "▸ app: SapuShell.app (Liquid Glass)"
SHELL_GLASS=$(measure shell-glass dist/SapuShell.app/Contents/MacOS/SapuShell sapu-shell.log)
echo "▸ app: SapuShell.app (SAPU_NO_GLASS=1)"
SHELL_PLAIN=$(measure shell-plain dist/SapuShell.app/Contents/MacOS/SapuShell sapu-shell.log SAPU_NO_GLASS=1)

size() { du -sk "$1" 2>/dev/null | awk '{printf "%.1f MB", $1/1024}'; }

# ── Report ──────────────────────────────────────────────────────────
{
  echo "# Sapu performance report"
  echo
  echo "- Machine: $(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m), $(sysctl -n hw.ncpu) cores, $(( $(sysctl -n hw.memsize) / 1073741824 )) GB RAM"
  echo "- macOS $(sw_vers -productVersion), commit $(git rev-parse --short HEAD), $(date -u '+%Y-%m-%d %H:%M UTC')"
  echo "- Test tree: $(find "$TREE" -type f | wc -l | tr -d ' ') files, $(du -sh "$TREE" | cut -f1) (generated, the same every run)"
  echo
  echo "## Engine (sapu-core, no UI)"
  echo
  echo "\`sapu scan\`, three runs (the first is a cold-ish cache):"
  echo
  echo "| Run | Wall time | Peak memory |"
  echo "|---|---|---|"
  printf '%s' "$CLI_ROWS"
  echo
  echo "What SapuShell calls through sapu-ffi (\`scan_bench\`; scan includes judging and recommendations):"
  echo
  echo '```json'
  echo "$FFI_JSON"
  echo '```'
  echo
  echo "## Apps"
  echo
  echo "Launched with the test tree as the folder to scan, each with an empty HOME (so no cached scan)."
  echo "\"Launch → scan done\" includes starting the app, scanning, judging and training Sapu AI."
  echo "Idle CPU is the average of 20 samples over 10 s after the scan, with the window open."
  echo
  echo "| App | Launch → scan done | Peak memory | Settled memory | Idle CPU |"
  echo "|---|---|---|---|---|"
  echo "| Sapu.app (egui) | ${SAPU%% |*} ms |${SAPU#* |} |"
  echo "| SapuShell.app, Liquid Glass | ${SHELL_GLASS%% |*} ms |${SHELL_GLASS#* |} |"
  echo "| SapuShell.app, no glass | ${SHELL_PLAIN%% |*} ms |${SHELL_PLAIN#* |} |"
  echo
  echo "## Sizes"
  echo
  echo "| Item | Size |"
  echo "|---|---|"
  echo "| Sapu.app | $(size dist/Sapu.app) |"
  echo "| SapuShell.app | $(size dist/SapuShell.app) |"
  echo "| \`sapu\` command | $(size target/release/sapu) |"
} > "$REPORT"

echo "✓ $REPORT"
cat "$REPORT"
