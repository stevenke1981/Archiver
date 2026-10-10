#!/usr/bin/env bash
# Archiver benchmark: compress / list / extract a ~500 MB mixed data set with the rarc CLI.
# Usage: bench/run.sh [path-to-rarc] [data-dir] [work-dir]
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
RARC=${1:-$HERE/../target/release/rarc}
DATA=${2:-/tmp/bench-data}
WORK=${3:-/tmp/bench-work}
FORMATS=${FORMATS:-"zip 7z tar.gz tar.xz tar.zst"}
[ -d "$DATA" ] || python3 "$HERE/gen_data.py" "$DATA"
rm -rf "$WORK"; mkdir -p "$WORK"
SRC=$(du -sb "$DATA" | cut -f1)
t() { /usr/bin/time -f "%e %M" "$@" 2>&1 >/dev/null | tail -1; }
printf "%-8s %10s %8s %9s %9s %9s %9s\n" format size ratio create_s list_s extract_s peakMB
for f in $FORMATS; do
  out="$WORK/bench.$f"
  read -r c cm < <(t "$RARC" c "$out" "$DATA")
  sz=$(stat -c %s "$out")
  read -r l _ < <(t "$RARC" l "$out")
  read -r x xm < <(t "$RARC" x "$out" -o "$WORK/x")
  diff -rq "$DATA" "$WORK/x/$(basename "$DATA")" >/dev/null || echo "!! $f: extracted data differs"
  rm -rf "$WORK/x" "$out"
  pk=$(( (cm > xm ? cm : xm) / 1024 ))
  printf "%-8s %10s %7.1f%% %9s %9s %9s %9s\n" "$f" "$((sz/1048576))MB" "$(awk "BEGIN{print 100*$sz/$SRC}")" "$c" "$l" "$x" "$pk"
done
ls -l "$(dirname "$RARC")"/archiver "$(dirname "$RARC")"/rarc 2>/dev/null | awk '{print $5, $9}'
