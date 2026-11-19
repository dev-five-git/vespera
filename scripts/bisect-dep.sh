#!/usr/bin/env bash
# Bisect a performance (or any) regression across the published versions of ONE
# dependency, using `cargo update -p <crate> --precise <ver>`.
#
#   scripts/bisect-dep.sh <crate> <good-version> <bad-version> <probe-command...>
#
# Like `git bisect run`, but over crates.io releases instead of commits. The
# probe command decides good/bad by exit code: 0 = good, non-zero = bad. For a
# perf regression make the probe fail when the number is too high, e.g. a script
# that runs `cargo bench ... -- --baseline main` and applies the same rule as
# scripts/bench-interleaved.sh: the median of the paired per-round changes above
# +30%, with every round above +20%.
#
# - Candidates come from the crates.io API (needs curl + jq): non-yanked,
#   non-prerelease releases between <good> and <bad> inclusive, semver-sorted.
# - Each probe runs `cargo update -p <crate> --precise <candidate>`, so the crate
#   must appear once in Cargo.lock and its Cargo.toml requirement must allow the
#   candidate (widen it temporarily to cross a 0.x minor / major boundary).
# - Cargo.lock is backed up first and ALWAYS restored on exit.
# - <good> must really be good and <bad> really bad; they are not re-verified.
#
# Example:
#   scripts/bisect-dep.sh hyper 1.8.0 1.12.0 \
#     cargo test -p vespera_inprocess --test alloc_budget
set -euo pipefail

if [ "$#" -lt 4 ]; then
  sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
fi
crate=$1 good=$2 bad=$3
shift 3

root=$(cd "$(dirname "$0")/.." && pwd)
lock="$root/Cargo.lock"
backup="$lock.bisect-backup"

# Fetched and parsed outside the process substitution below: `set -e` cannot
# see a failure inside `< <(...)`, which would otherwise surface as a
# misleading "good must be older than bad" error.
json=$(curl -fsS -A 'vespera-bisect-dep (https://github.com/dev-five-git/vespera)' \
  "https://crates.io/api/v1/crates/$crate/versions") || {
  echo "crates.io lookup failed for '$crate' (network error or unknown crate)" >&2
  exit 2
}
nums=$(jq -r '.versions[] | select((.yanked | not) and (.num | test("-") | not)) | .num' <<<"$json") || {
  echo "unexpected crates.io response for '$crate'" >&2
  exit 2
}
mapfile -t versions < <(sed 's/+.*//' <<<"$nums" | sed '/^$/d' | sort -uV)
if [ "${#versions[@]}" -eq 0 ]; then
  echo "crates.io lists no stable, non-yanked release of '$crate'" >&2
  exit 2
fi

index_of() {
  local i
  for i in "${!versions[@]}"; do
    [ "${versions[$i]}" = "$1" ] && { echo "$i"; return; }
  done
  echo -1
}
lo=$(index_of "$good")
hi=$(index_of "$bad")
if [ "$lo" -lt 0 ] || [ "$hi" -lt 0 ] || [ "$lo" -ge "$hi" ]; then
  echo "good ($good) must be an older published release than bad ($bad) for '$crate'" >&2
  exit 2
fi

cp "$lock" "$backup"
trap 'mv -f "$backup" "$lock"' EXIT

while [ $((hi - lo)) -gt 1 ]; do
  mid=$(((lo + hi) / 2))
  candidate=${versions[$mid]}
  echo "==> probing $crate $candidate (good<=${versions[$lo]}, bad>=${versions[$hi]})"
  cargo update --manifest-path "$root/Cargo.toml" -p "$crate" --precise "$candidate" ||
    { echo "cargo update to $candidate failed; widen the version requirement." >&2; exit 2; }
  if (cd "$root" && "$@"); then lo=$mid; else hi=$mid; fi
done
echo "RESULT: last good $crate ${versions[$lo]}; first bad $crate ${versions[$hi]}"
