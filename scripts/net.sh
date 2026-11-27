#!/usr/bin/env bash
set -euo pipefail

# Monitor Alacritty and velm (server + client) network connections via lsof -i -P -n
# over N minutes (default: 10), polling every 5 seconds. Excludes pane shells/children.

if [[ "$(uname -s)" != "Darwin" ]] && [[ "$(uname -s)" != "Linux" ]]; then
  echo "error: scripts/net.sh requires macOS or Linux (lsof)" >&2
  exit 2
fi

for cmd in lsof pgrep awk sort; do
  command -v "$cmd" >/dev/null || { echo "error: required command not found: $cmd" >&2; exit 1; }
done

minutes=${1:-10}
interval=${VELM_NET_INTERVAL_SECONDS:-5}
total_seconds=${VELM_NET_DURATION_SECONDS:-$((minutes * 60))}

tmp_log=$(mktemp /tmp/velm-net.XXXXXX)
cleanup() { rm -f "$tmp_log"; }
trap cleanup EXIT

collect_pids() {
  local pids=()
  while IFS= read -r pid; do
    [[ -n "$pid" ]] && pids+=("$pid")
  done < <(pgrep -x 'alacritty|velm' 2>/dev/null || true)
  if (( ${#pids[@]} > 0 )); then
    (IFS=,; echo "${pids[*]}")
  fi
}

echo "Monitoring alacritty/velm network sockets for ${total_seconds}s (every ${interval}s)..."
elapsed=0
while (( elapsed <= total_seconds )); do
  pid_list=$(collect_pids || true)
  if [[ -n "$pid_list" ]]; then
    lsof -i -P -n -a -p "$pid_list" 2>/dev/null | awk 'NR > 1 { print $1, $8, $9, $10 }' >> "$tmp_log" || true
  fi
  if (( elapsed >= total_seconds )); then
    break
  fi
  sleep "$interval"
  elapsed=$((elapsed + interval))
done

unique_connections=$(sort -u "$tmp_log")
if [[ -z "$unique_connections" ]]; then
  echo "OK: 0 network connections observed for alacritty/velm."
  exit 0
fi

echo "Unique network connections observed:"
printf '%s\n' "$unique_connections"
exit 1
