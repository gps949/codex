#!/usr/bin/env bash
# Preserve the command's exit status alongside lightweight hosted-runner diagnostics.
set -euo pipefail
label="${1:?expected a diagnostic label}"
shift
if [[ ! "$label" =~ ^[a-z0-9-]+$ || $# -eq 0 ]]; then
  echo "Expected a simple label followed by a command" >&2
  exit 2
fi
diagnostic_dir="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/fork-ci-diagnostics"
mkdir -p "$diagnostic_dir"
resources="$diagnostic_dir/$label-resources.log"

snapshot() {
  echo "Resource sample: $label"
  date -u '+%Y-%m-%dT%H:%M:%SZ'
  df -h . "$diagnostic_dir" || true
  if [[ -r /proc/meminfo ]]; then
    awk '/^(MemTotal|MemAvailable|SwapTotal|SwapFree):/ {print}' /proc/meminfo
    ps -eo pid,ppid,comm,rss --sort=-rss | head -n 9 || true
  fi
  for counter in /proc/pressure/memory /sys/fs/cgroup/memory.current /sys/fs/cgroup/memory.max /sys/fs/cgroup/memory.events; do
    if [[ -r "$counter" ]]; then
      echo "$counter"
      cat "$counter" || true
    fi
  done
}

snapshot | tee -a "$resources"
(
  while sleep 15; do
    snapshot | tee -a "$resources"
  done
) 2>&1 &
monitor_pid=$!
trap 'kill "$monitor_pid" 2>/dev/null || true; wait "$monitor_pid" 2>/dev/null || true' EXIT

set +e
"$@" 2>&1 | tee "$diagnostic_dir/$label.log"
pipeline_status=("${PIPESTATUS[@]}")
set -e
status="${pipeline_status[0]}"
if [[ "$status" -eq 0 && "${pipeline_status[1]}" -ne 0 ]]; then
  status="${pipeline_status[1]}"
fi
printf '%s\n' "$status" > "$diagnostic_dir/$label.exit"
snapshot >> "$resources" 2>&1
tail -n 45 "$resources"
echo "CI command $label exited with status $status"
exit "$status"
