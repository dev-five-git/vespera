#!/usr/bin/env bash
# Interleaved Criterion regression gate: merge-base vs. the current checkout.
#
#   scripts/bench-interleaved.sh <base-rev> [bench-filter-regex]
#
# Why: shared CI runners drift (noisy neighbours, frequency scaling, cache
# state) on a minutes scale. Measuring the baseline in one block and the PR in
# a later block turns that drift into a fake "regression" (PR #144, whose two
# sides held byte-identical Rust, swung -12%..+15% and tripped a different
# bench each run). Here both sides are built once, then measured in ROUNDS
# alternating rounds (base,head / head,base / base,head / ...), each side in
# its own CRITERION_HOME so no state leaks between them. Within a round the two
# measurements are seconds apart and see the same machine conditions, so the
# PAIRED per-round change  c_i = head_mean_i / base_mean_i - 1  cancels drift.
#
# Decision per benchmark (the same thresholds as before, now on paired data):
#   FAIL  iff  median(c_i) > REGRESS_MEAN (+30%)   -- the "mean change" estimate
#         AND  min(c_i)    > REGRESS_LOWER (+20%)  -- the confidence lower bound
# min of N paired samples is the distribution-free lower confidence limit of
# the median (sign test: 1 - 2/2^N, i.e. 93.75% for N=5): a regression must
# show up in EVERY round, so one noisy round can no longer trip the gate, while
# a real >30% slowdown (present in every round) still does.
#
# Env: ROUNDS (5) WARMUP_SECS (1) MEASURE_SECS (2) REGRESS_MEAN (0.30)
#      REGRESS_LOWER (0.20) WORK_DIR (mktemp)
# Needs: git, cargo, jq, awk, sort. Exit 1 when any benchmark regresses.
set -euo pipefail

if [ "$#" -lt 1 ]; then
  sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
fi
base_rev=$1
filter=${2:-${BENCH_FILTER:-}}
rounds=${ROUNDS:-5}
warmup=${WARMUP_SECS:-1}
measure=${MEASURE_SECS:-2}
limit_mean=${REGRESS_MEAN:-0.30}
limit_lower=${REGRESS_LOWER:-0.20}

repo=$(cd "$(dirname "$0")/.." && pwd)
work=${WORK_DIR:-$(mktemp -d)}
base_tree="$work/base-tree"

git -C "$repo" worktree add --detach "$base_tree" "$base_rev" >/dev/null
trap 'git -C "$repo" worktree remove --force "$base_tree" >/dev/null 2>&1 || true' EXIT

# Each side gets its OWN fresh target dir. A shared (or pre-existing) one is
# unsafe: cargo keys workspace members relative to the workspace root, so both
# trees map to one fingerprint and the second build is judged "fresh" by mtime
# -- silently benchmarking the same binary on both sides.
build_side() { # <source-tree> <target-dir>
  (cd "$1" && cargo bench -p vespera_inprocess --bench dispatch --no-run \
    --message-format=json --target-dir "$2" |
    jq -r 'select(.reason == "compiler-artifact" and .target.name == "dispatch"
                  and (.target.kind | index("bench")) and .executable != null)
           | .executable' | tail -n 1)
}
base_bin=$(build_side "$base_tree" "$work/target-base")
head_bin=$(build_side "$repo" "$work/target-head")
[ "$base_bin" != "$head_bin" ] || { echo "base and head resolved to one binary" >&2; exit 1; }
echo "base: $base_rev  ($base_bin)"
echo "head: $(git -C "$repo" rev-parse HEAD)  ($head_bin)"

run_side() { # <side> <round> <binary> <package-dir>
  (cd "$4" && CRITERION_HOME="$work/criterion/$1/$2" "$3" --bench \
    --noplot --warm-up-time "$warmup" --measurement-time "$measure" ${filter:+"$filter"})
}
for round in $(seq 1 "$rounds"); do
  echo "::group::round $round/$rounds"
  if [ $((round % 2)) -eq 1 ]; then order="base head"; else order="head base"; fi
  for side in $order; do
    if [ "$side" = base ]; then bin=$base_bin; dir=$base_tree/crates/vespera_inprocess
    else bin=$head_bin; dir=$repo/crates/vespera_inprocess; fi
    run_side "$side" "$round" "$bin" "$dir" >"$work/$side-$round.log" 2>&1 ||
      { cat "$work/$side-$round.log"; exit 1; }
  done
  echo "::endgroup::"
done

mean_of() { jq -r '.mean.point_estimate' "$1"; }
fail=0
found=0
while IFS= read -r est; do
  bench=${est#"$work/criterion/head/1/"}
  bench=${bench%/new/estimates.json}
  changes=""
  for round in $(seq 1 "$rounds"); do
    h="$work/criterion/head/$round/$bench/new/estimates.json"
    b="$work/criterion/base/$round/$bench/new/estimates.json"
    [ -f "$h" ] && [ -f "$b" ] || continue
    changes+=$(awk -v h="$(mean_of "$h")" -v b="$(mean_of "$b")" 'BEGIN{printf "%.6f\n", h/b-1}')$'\n'
  done
  [ -n "$changes" ] || { echo "::notice::$bench exists on one side only; not gated."; continue; }
  found=1
  sorted=$(printf '%s' "$changes" | sort -g)
  read -r median lower < <(printf '%s\n' "$sorted" | awk '
    {v[NR]=$1}
    END{m=(NR%2)?v[(NR+1)/2]:(v[NR/2]+v[NR/2+1])/2; print m, v[1]}')
  printf '%s: median %+.2f%% (min of %d paired rounds %+.2f%%)  [%s]\n' "$bench" \
    "$(awk -v v="$median" 'BEGIN{print v*100}')" "$(printf '%s\n' "$sorted" | wc -l)" \
    "$(awk -v v="$lower" 'BEGIN{print v*100}')" "$(printf '%s' "$changes" | tr '\n' ' ')"
  if awk -v m="$median" -v l="$lower" -v lm="$limit_mean" -v ll="$limit_lower" \
    'BEGIN{exit !(m > lm && l > ll)}'; then
    echo "::error::Performance regression: ${bench} median change exceeds ${limit_mean} with every paired round > ${limit_lower}"
    fail=1
  fi
done < <(find "$work/criterion/head/1" -path '*/new/estimates.json' | sort)

[ "$found" -eq 1 ] || echo "::notice::No comparable benchmarks found; nothing to gate."
exit $fail
