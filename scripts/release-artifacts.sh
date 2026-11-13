#!/usr/bin/env bash
# Collect what a release just published, for provenance + SBOM attestation.
#
#   scripts/release-artifacts.sh <published-json> <out-dir>
#
# <published-json> is the changepacks action's `changepacks` output: a JSON array
# of the project paths it published in this run, e.g.
#   ["crates/vespera/Cargo.toml","libs/vespera-bridge/build.gradle.kts"]
#
# Runs ONLY in the main-only release job, right after changepacks published, and
# never publishes or attests anything itself. It writes:
#   <out-dir>/artifacts/   the published files: <crate>-<v>.crate (the tarball
#                          `cargo publish` uploaded, checked against the SHA-256
#                          crates.io reports) and the Maven JARs (main, sources,
#                          javadoc) the Gradle publish task built
#   <out-dir>/sbom/        the matching CycloneDX SBOMs (scripts/sbom.sh)
#   <out-dir>/manifest.tsv one line per package, tab-separated: project path,
#                          version, id (crate name, or vespera_bridge /
#                          vespera_bridge_gradle_plugin), then the artifact and
#                          SBOM paths RELATIVE to <out-dir> (the directory is
#                          handed to the separate `attest` job as an artifact)
# and, under GitHub Actions, the step output `any=true|false` (whether at least
# one package was collected). A package that cannot be collected (checksum
# mismatch, missing artifact) is reported and skipped; the others are still
# collected, and the script then exits non-zero.
#
# The SBOMs must describe the RELEASED commit. changepacks publishes from the
# merge commit of the "Update Versions" PR, which is usually but not always the
# commit this workflow runs on, so the commit is read from the published crate
# tarball's .cargo_vcs_info.json (HEAD when no crate was published) and the
# SBOMs are generated in a temporary worktree of it.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
fi
published=$1
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$2/artifacts" "$2/sbom"
out=$(cd "$2" && pwd)
: >"$out/manifest.tsv"
output() { if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "$1" >>"$GITHUB_OUTPUT"; fi; }

mapfile -t paths < <(jq -r 'if type == "array" then .[] else empty end' <<<"${published:-[]}" | tr -d '\r')
if [ "${#paths[@]}" -eq 0 ]; then
  echo "nothing was published in this run"
  output any=false
  exit 0
fi
printf 'published: %s\n' "${paths[@]}"

# ── The released commit ───────────────────────────────────────────────────
sha=$(git -C "$root" rev-parse HEAD)
for p in "${paths[@]}"; do
  if [[ $p =~ ^crates/([a-z_]+)/Cargo\.toml$ ]]; then
    for crate_file in "$root"/target/package/"${BASH_REMATCH[1]}"-*.crate; do
      [ -f "$crate_file" ] || continue
      vcs=$(tar -xzOf "$crate_file" --wildcards '*/.cargo_vcs_info.json' 2>/dev/null |
        jq -r '.git.sha1 // empty' || true)
      if [ -n "$vcs" ]; then sha=$vcs; break 2; fi
    done
  fi
done
# Always a separate worktree, even when the released commit is HEAD: resolving
# the dependency graph rewrites a stale Cargo.lock, and the checkout must stay
# clean for the release job's next step (Cargo.lock sync on the version PR).
git -C "$root" cat-file -e "$sha^{commit}" 2>/dev/null ||
  git -C "$root" fetch --no-tags --depth=1 origin "$sha"
src=$(mktemp -d)
bridge_published_sbom=$(mktemp -d)
# Cleanup must never turn a successful run into a failed one (`set -e` also
# applies inside an EXIT trap), hence the `|| true`s.
trap 'git -C "$root" worktree remove --force "$src" || true; rm -rf "$src" "$bridge_published_sbom" || true' EXIT
git -C "$root" worktree add --detach "$src" "$sha"
echo "released commit: $sha"

# The bridge SBOM that went to Maven Central (classifier `cyclonedx`) was built
# by the publish task; keep that exact file rather than a regenerated one
# (fresh serial number).
cp "$root"/libs/vespera-bridge/build/reports/cyclonedx-direct/*-cyclonedx.json \
  "$bridge_published_sbom/" 2>/dev/null || true

bash "$root/scripts/sbom.sh" "$out/sbom-all" "$src"
metadata=$(cargo metadata --manifest-path "$src/Cargo.toml" --no-deps --format-version 1)

failed=0
fail() { echo "::error::$1"; failed=1; }
collected=0
record() { # <path> <version> <id> <artifact> <sbom>
  cp "$5" "$out/sbom/"
  printf '%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "artifacts/$(basename "$4")" \
    "sbom/$(basename "$5")" >>"$out/manifest.tsv"
  collected=$((collected + 1))
}

crates_io_checksum() { # <crate> <version>
  local attempt sum
  for attempt in 1 2 3 4 5 6; do
    sum=$(curl -fsS -A 'vespera-release (https://github.com/dev-five-git/vespera)' \
      "https://crates.io/api/v1/crates/$1/$2" | jq -r '.version.checksum // empty') || true
    if [ -n "$sum" ]; then echo "$sum"; return 0; fi
    if [ "$attempt" -lt 6 ]; then sleep 10; fi
  done
  return 1
}

for p in "${paths[@]}"; do
  case $p in
    crates/*/Cargo.toml)
      crate=${p#crates/}; crate=${crate%/Cargo.toml}
      version=$(jq -r --arg n "$crate" '.packages[] | select(.name == $n) | .version' <<<"$metadata" | tr -d '\r')
      file="$root/target/package/$crate-$version.crate"
      if [ ! -f "$file" ]; then
        # Checked against crates.io below, so a non-identical repackage is
        # refused rather than attested.
        echo "::warning::$crate-$version.crate not left by cargo publish; repackaging"
        file="$src/target/package/$crate-$version.crate"
        if ! cargo package --no-verify --manifest-path "$src/Cargo.toml" -p "$crate"; then
          fail "could not repackage $crate@$version; not attesting it"
          continue
        fi
      fi
      if ! want=$(crates_io_checksum "$crate" "$version"); then
        fail "could not read the crates.io checksum of $crate@$version; not attesting it"
        continue
      fi
      got=$(sha256sum "$file" | cut -d' ' -f1)
      if [ "$got" != "$want" ]; then
        fail "$crate-$version.crate sha256 $got != crates.io $want; not attesting it"
        continue
      fi
      cp "$file" "$out/artifacts/"
      record "$p" "$version" "$crate" "$file" "$out/sbom-all/$crate-$version.cdx.json"
      ;;
    libs/*/build.gradle.kts)
      lib=${p#libs/}; lib=${lib%/build.gradle.kts}
      version=$(tr -d '\r' <"$src/$p" | sed -nE 's/^version = "([^"]+)"$/\1/p')
      libs_dir="$root/libs/$lib/build/libs"
      if [ ! -f "$libs_dir/$lib-$version.jar" ]; then
        # Gradle 9 builds reproducible archives by default, so a rebuild of
        # the released commit should match what was uploaded (NOT verified
        # against Maven Central, unlike the crates).
        echo "::warning::$lib-$version.jar not left by the publish task; rebuilding"
        libs_dir="$src/libs/$lib/build/libs"
        if ! (cd "$src/libs/$lib" && sh ./gradlew publishToMavenLocal --console=plain --no-daemon -q) ||
          [ ! -f "$libs_dir/$lib-$version.jar" ]; then
          fail "could not rebuild $lib-$version.jar; not attesting it"
          continue
        fi
      fi
      for jar in "$libs_dir/$lib-$version"{,-sources,-javadoc}.jar; do
        if [ -f "$jar" ]; then cp "$jar" "$out/artifacts/"; fi
      done
      sbom="$out/sbom-all/$lib-$version-cyclonedx.json"
      if [ -f "$bridge_published_sbom/$lib-$version-cyclonedx.json" ]; then
        sbom="$bridge_published_sbom/$lib-$version-cyclonedx.json"
      fi
      record "$p" "$version" "${lib//-/_}" "$libs_dir/$lib-$version.jar" "$sbom"
      ;;
    *)
      echo "::warning::unknown published path $p; not attested"
      ;;
  esac
done
rm -rf "$out/sbom-all"

if [ "$collected" -gt 0 ]; then output any=true; else output any=false; fi
echo "── release artifacts"
(cd "$out" && sha256sum artifacts/* sbom/* 2>/dev/null) || true
exit "$failed"
