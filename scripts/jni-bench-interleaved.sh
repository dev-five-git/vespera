#!/usr/bin/env bash
# Interleaved JNI dispatch regression gate: merge-base vs. the current checkout.
#
#   scripts/jni-bench-interleaved.sh <base-rev>
#
# The JNI counterpart of scripts/bench-interleaved.sh (criterion). Both sides
# are built once -- the rust-jni-demo cdylib (release, fat LTO, pinned code
# alignment) plus the vespera-bridge JAR -- then the harness
# libs/vespera-bridge/bench/JniDispatchBench.java (taken from the CURRENT
# checkout, compiled against each side's JAR) runs in ROUNDS rounds
# alternating base,head / head,base / ..., ONE FRESH JVM per side per round
# (a JVM can load a vespera cdylib only once). Each JVM prints the median
# block ns/op of every mode; scripts/jni-bench-compare.ts pairs round i of
# both sides and applies the criterion gate's rule:
#   FAIL  iff  median(c_i) > REGRESS_MEAN (+30%)  AND  min(c_i) > REGRESS_LOWER (+20%)
# with c_i = head_i / base_i - 1.
#
# Pinned alignment: under fat LTO + one codegen unit an unrelated change
# shifts function / loop alignment and swung identical code by up to +/-40%
# (PR #161), so both cdylibs are built with JNI_BENCH_RUSTFLAGS (default:
# 64 B function / 32 B non-fallthrough block alignment). It REPLACES any
# ambient RUSTFLAGS (setup-rust-toolchain exports `-D warnings`).
#
# Env: ROUNDS (5) CARGO_JOBS (cargo default) WORK_DIR (mktemp)
#      JNI_BENCH_RUSTFLAGS  JAVA_HOME (else java/javac on PATH)
#      BENCH_JVM_OPTS ("-Xms256m -Xmx256m -XX:+UseParallelGC")
#      BENCH_WARMUP_MS (1500) BENCH_BLOCK_MS (20) BENCH_BLOCKS (40)
#      REGRESS_MEAN (0.30) REGRESS_LOWER (0.20)
#      GITHUB_STEP_SUMMARY (markdown report appended when set)
# Needs: git, cargo, a JDK 17+, bun. Exit 1 when any mode regresses.
set -euo pipefail

if [ "$#" -lt 1 ]; then
  sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
fi
base_rev=$1
rounds=${ROUNDS:-5}
export RUSTFLAGS=${JNI_BENCH_RUSTFLAGS:-"-C llvm-args=-align-all-functions=6 -C llvm-args=-align-all-nofallthru-blocks=5"}

repo=$(cd "$(dirname "$0")/.." && pwd)
work=${WORK_DIR:-$(mktemp -d)}
mkdir -p "$work/results"
base_tree="$work/base-tree"

if [ -n "${JAVA_HOME:-}" ]; then
  java="$JAVA_HOME/bin/java"
  javac="$JAVA_HOME/bin/javac"
else
  java=java
  javac=javac
fi

# Git Bash / MSYS: hand native Windows paths to the JDK.
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) cp_sep=';'; to_native() { cygpath -w "$1"; } ;;
  *) cp_sep=':'; to_native() { printf '%s' "$1"; } ;;
esac

git -C "$repo" -c core.longpaths=true worktree add --detach "$base_tree" "$base_rev" >/dev/null
trap 'git -C "$repo" worktree remove --force "$base_tree" >/dev/null 2>&1 || true' EXIT

# Each side gets its OWN target dir: cargo keys workspace members relative to
# the workspace root, so a shared one would judge the second build fresh and
# benchmark one cdylib against itself (see scripts/bench-interleaved.sh).
build_side() { # <side> <source-tree>
  local side=$1 tree=$2 lib
  echo "::group::build $side"
  (cd "$tree" && cargo build -p rust-jni-demo --lib --release ${CARGO_JOBS:+-j "$CARGO_JOBS"} \
    --target-dir "$work/target-$side")
  mkdir -p "$work/lib-$side"
  for lib in librust_jni_demo.so rust_jni_demo.dll librust_jni_demo.dylib; do
    [ -f "$work/target-$side/release/$lib" ] && cp "$work/target-$side/release/$lib" "$work/lib-$side/"
  done
  [ -n "$(ls -A "$work/lib-$side")" ] || { echo "no cdylib built for $side" >&2; exit 1; }

  (cd "$tree/libs/vespera-bridge" && sh ./gradlew jar --console=plain -q)
  local jar
  jar=$(find "$tree/libs/vespera-bridge/build/libs" -name 'vespera-bridge-*.jar' \
    ! -name '*-sources.jar' ! -name '*-javadoc.jar' | head -n 1)
  [ -n "$jar" ] || { echo "no vespera-bridge jar built for $side" >&2; exit 1; }
  cp "$jar" "$work/bridge-$side.jar"

  mkdir -p "$work/classes-$side"
  "$javac" -encoding UTF-8 -d "$(to_native "$work/classes-$side")" \
    -cp "$(to_native "$work/bridge-$side.jar")" \
    "$(to_native "$repo/libs/vespera-bridge/bench/JniDispatchBench.java")"
  echo "::endgroup::"
}
build_side base "$base_tree"
build_side head "$repo"
cmp -s "$work/lib-base/"* "$work/lib-head/"* &&
  echo "::notice::base and head cdylibs are byte-identical (expected only for JNI-irrelevant changes)."
echo "base: $(git -C "$base_tree" rev-parse HEAD)"
echo "head: $(git -C "$repo" rev-parse HEAD)"

run_side() { # <side> <round>
  local side=$1 round=$2
  # shellcheck disable=SC2086 # BENCH_JVM_OPTS is a word list
  "$java" ${BENCH_JVM_OPTS:--Xms256m -Xmx256m -XX:+UseParallelGC} \
    -Djava.library.path="$(to_native "$work/lib-$side")" \
    -Dvespera.native.cleanupLegacy=false \
    -Dbench.warmupMillis="${BENCH_WARMUP_MS:-1500}" \
    -Dbench.blockMillis="${BENCH_BLOCK_MS:-20}" \
    -Dbench.blocks="${BENCH_BLOCKS:-40}" \
    -cp "$(to_native "$work/bridge-$side.jar")$cp_sep$(to_native "$work/classes-$side")" \
    JniDispatchBench >"$work/$side-$round.out" 2>"$work/$side-$round.log" ||
    { cat "$work/$side-$round.out" "$work/$side-$round.log"; exit 1; }
  grep '^RESULT ' "$work/$side-$round.out" | cut -d' ' -f2- >"$work/results/$side-$round.txt" ||
    { cat "$work/$side-$round.out" "$work/$side-$round.log"; echo "::error::$side round $round printed no RESULT lines" >&2; exit 1; }
  echo "[$side round $round]"
  cat "$work/$side-$round.log"
}
for round in $(seq 1 "$rounds"); do
  echo "::group::round $round/$rounds"
  if [ $((round % 2)) -eq 1 ]; then order="base head"; else order="head base"; fi
  for side in $order; do run_side "$side" "$round"; done
  echo "::endgroup::"
done

bun "$repo/scripts/jni-bench-compare.ts" "$work/results"
