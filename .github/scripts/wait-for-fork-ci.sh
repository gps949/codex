#!/usr/bin/env bash
# Gate a release on the newest fork CI push run for precisely its source commit.
set -euo pipefail
repo="$1"
sha="$2"
attempts="${3:-120}"
delay="${4:-60}"
for ((attempt=1; attempt<=attempts; attempt++)); do
  result="$(gh run list --repo "$repo" --workflow fork-ci.yml \
    --branch feature/native-multi-account --event push --commit "$sha" --limit 1 \
    --json headSha,status,conclusion --jq '.[0] | [.headSha, .status, .conclusion] | join(" ")')"
  read -r actual status conclusion <<< "$result"
  if [ -n "$actual" ] && [ "$actual" != "$sha" ]; then
    echo "CI returned an unexpected commit: $actual" >&2
    exit 1
  fi
  if [ "${status:-}" = completed ]; then
    if [ "${conclusion:-}" = success ]; then
      echo "fork-ci passed for $sha"
      exit 0
    fi
    echo "fork-ci did not pass for $sha: ${conclusion:-unknown}" >&2
    exit 1
  fi
  echo "Waiting for fork-ci on $sha ($attempt/$attempts)."
  if [ "$attempt" -lt "$attempts" ]; then sleep "$delay"; fi
done
echo "No successful fork-ci run completed for $sha before the deadline." >&2
exit 1
