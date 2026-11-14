#!/usr/bin/env bash
# Collect what a release just published, for provenance + SBOM attestation.
#
#   scripts/release-artifacts.sh <published-json> <out-dir>
#   scripts/release-artifacts.sh --from-registries <version> <out-dir>
#
# --from-registries (the workflow_dispatch `reattest` workflow, for a release
# whose attestation failed or predates it): all seven packages of <version>,
# downloaded from crates.io (SHA-256 checked against the crates.io API), Maven
# Central and the Gradle Plugin Portal (SHA-1 checked against the registry's
# .sha1) instead of the publish run's build outputs. The released commit is the
# one every changepacks tag `<name>(<project path>)@<version>` points to (all
# seven must agree, and must match each crate's .cargo_vcs_info.json).
#
# <published-json> is the changepacks action's `changepacks` output: a JSON array
# of the project paths it published in this run, e.g.
#   ["crates/vespera/Cargo.toml","libs/vespera-bridge/build.gradle.kts"]
#
# Runs ONLY in the main-only release job, right after changepacks published, and
# never publishes or attests anything itself. It writes:
#   <out-dir>/artifacts/   the published files: <crate>-<v>.crate (the tarball
#                          `cargo publish` uploaded, or else the one crates.io
#                          serves; either way checked against the SHA-256
#                          crates.io reports) and the Maven JARs (main, sources,
#                          javadoc) the Gradle publish task built
#   <out-dir>/sbom/        the matching CycloneDX SBOMs (scripts/sbom.sh)
#   <out-dir>/manifest.tsv one line per package, tab-separated: project path,
#                          version, id (crate name, or vespera_bridge /
#                          vespera_bridge_gradle_plugin), then the artifact and
#                          SBOM paths RELATIVE to <out-dir> (the directory is
#                          handed to the separate `attest` job as an artifact)
# and, under GitHub Actions, the step output `any=true|false` (whether at least
# one package was collected; written on exit, so also after a failure). Every
# per-package step (download, checksum, JAR, SBOM) is isolated: a package that
# cannot be collected is reported and skipped, the others are still collected,
# and the script then exits non-zero.
#
# The SBOMs must describe the RELEASED commit. changepacks publishes from the
# merge commit of the "Update Versions" PR, which is usually but not always the
# commit this workflow runs on, so the commit is read from the published crate
# tarballs' .cargo_vcs_info.json (all of them must agree; HEAD only when no
# crate was published) and the SBOMs are generated in a temporary worktree of
# it. Every collected crate must have been packaged from that commit.
set -euo pipefail

usage() { sed -n '2,43p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }
registry_version=
if [ "${1:-}" = --from-registries ]; then
  [ "$#" -eq 3 ] || usage
  registry_version=$2
  [[ $registry_version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    echo "::error::invalid version '$registry_version'" >&2
    exit 2
  }
  shift 2
  set -- '["crates/vespera/Cargo.toml","crates/vespera_core/Cargo.toml","crates/vespera_macro/Cargo.toml","crates/vespera_inprocess/Cargo.toml","crates/vespera_jni/Cargo.toml","libs/vespera-bridge/build.gradle.kts","libs/vespera-bridge-gradle-plugin/build.gradle.kts"]' "$1"
fi
[ "$#" -eq 2 ] || usage
published=$1
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$2/artifacts" "$2/sbom"
out=$(cd "$2" && pwd)
: >"$out/manifest.tsv"
output() { if [ -n "${GITHUB_OUTPUT:-}" ]; then echo "$1" >>"$GITHUB_OUTPUT"; fi; }

failed=0
fail() { echo "::error::$1"; failed=1; }
collected=0
downloads=$(mktemp -d)
src=
bridge_published_sbom=
# On EVERY exit (also an unexpected one), publish `any` so that the upload step
# still hands whatever was collected to the attest job, and clean up. Cleanup
# must never turn a successful run into a failed one (`set -e` also applies
# inside an EXIT trap), hence the `|| true`s.
cleanup() {
  if [ "$collected" -gt 0 ]; then output any=true; else output any=false; fi
  if [ -n "$src" ]; then git -C "$root" worktree remove --force "$src" >/dev/null 2>&1 || true; fi
  rm -rf "$downloads" ${src:+"$src"} ${bridge_published_sbom:+"$bridge_published_sbom"} || true
}
trap cleanup EXIT

mapfile -t paths < <(jq -r 'if type == "array" then .[] else empty end' <<<"${published:-[]}" | tr -d '\r')
if [ "${#paths[@]}" -eq 0 ]; then
  echo "nothing was published in this run"
  exit 0
fi
printf 'published: %s\n' "${paths[@]}"

curl_get() { # <url> <file>: download with retries (registries can lag a publish)
  curl -fsSL --retry 5 --retry-delay 10 -A 'vespera-release (https://github.com/dev-five-git/vespera)' \
    -o "$2" "$1"
}

# Maven Central / the Plugin Portal can take ~30 min to serve a fresh
# release (0.7.0's re-attest started minutes after the publish and got 404s),
# so a registry download keeps trying for up to 30 minutes.
registry_get() { # <url> <file>
  local attempt
  for attempt in $(seq 1 30); do
    if curl_get "$1" "$2"; then return 0; fi
    if [ "$attempt" -lt 30 ]; then
      echo "waiting for $1 (attempt $attempt/30)" >&2
      sleep 60
    fi
  done
  return 1
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

vcs_sha_of() { # <crate file>: the commit `cargo package` recorded (empty if none)
  tar -xzOf "$1" --wildcards '*/.cargo_vcs_info.json' 2>/dev/null |
    jq -r '.git.sha1 // empty' 2>/dev/null || true
}

# ── The published crates: located, checksum-verified, commit read ──────────
# Done BEFORE the released commit is chosen: in a normal release that commit
# is read from these very tarballs.
declare -A crate_file=() crate_version=() crate_vcs=()
head_metadata=
if [ -z "$registry_version" ]; then
  head_metadata=$(cargo metadata --manifest-path "$root/Cargo.toml" --no-deps --format-version 1 2>/dev/null) ||
    head_metadata=
fi
for p in "${paths[@]}"; do
  [[ $p =~ ^crates/([a-z_]+)/Cargo\.toml$ ]] || continue
  crate=${BASH_REMATCH[1]}
  if [ -n "$registry_version" ]; then
    version=$registry_version
  else
    version=
    if [ -n "$head_metadata" ]; then
      version=$(jq -r --arg n "$crate" '.packages[] | select(.name == $n) | .version' \
        <<<"$head_metadata" | tr -d '\r') || version=
    fi
    if [ -z "$version" ]; then
      fail "could not read the version of $crate from Cargo metadata; not attesting it"
      continue
    fi
  fi
  file="$root/target/package/$crate-$version.crate"
  if [ -n "$registry_version" ] || [ ! -f "$file" ]; then
    # Newer cargo no longer leaves the uploaded .crate behind, and a repackage
    # is not byte-identical (0.7.0), so fetch what crates.io serves; it is
    # checked against the crates.io checksum below.
    [ -n "$registry_version" ] ||
      echo "::warning::$crate-$version.crate not left by cargo publish; downloading it from crates.io"
    file="$downloads/$crate-$version.crate"
    if ! curl_get "https://static.crates.io/crates/$crate/$crate-$version.crate" "$file"; then
      fail "could not download $crate@$version from crates.io; not attesting it"
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
  vcs=$(vcs_sha_of "$file")
  if [ -z "$vcs" ]; then
    fail "$crate-$version.crate records no git commit (.cargo_vcs_info.json); not attesting it"
    continue
  fi
  crate_file[$crate]=$file
  crate_version[$crate]=$version
  crate_vcs[$crate]=$vcs
done

# ── --from-registries: the published JARs ─────────────────────────────────
if [ -n "$registry_version" ]; then
  for p in "${paths[@]}"; do
    [[ $p =~ ^libs/([a-z-]+)/build\.gradle\.kts$ ]] || continue
    lib=${BASH_REMATCH[1]}
    repo=https://repo1.maven.org/maven2
    if [ "$lib" = vespera-bridge-gradle-plugin ]; then repo=https://plugins.gradle.org/m2; fi
    base="$repo/kr/devfive/$lib/$registry_version/$lib-$registry_version"
    for suffix in .jar -sources.jar -javadoc.jar; do
      f="$downloads/$lib-$registry_version$suffix"
      if registry_get "$base$suffix" "$f" && registry_get "$base$suffix.sha1" "$f.sha1" &&
        [ "$(sha1sum "$f" | cut -d' ' -f1)" = "$(tr -d ' \r\n' <"$f.sha1" | cut -c1-40)" ]; then
        rm "$f.sha1"
      else
        fail "could not download (or SHA-1-verify) $lib-$registry_version$suffix"
        rm -f "$f" "$f.sha1"
      fi
    done
  done
fi

# ── The released commit ───────────────────────────────────────────────────
head_sha=$(git -C "$root" rev-parse HEAD)
sha=
if [ -n "$registry_version" ]; then
  # Every changepacks tag of the version must exist and name the same commit.
  tags=$(git -C "$root" ls-remote --tags origin)
  for p in "${paths[@]}"; do
    name=${p#*/}; name=${name%%/*}
    ref="refs/tags/$name($p)@$registry_version"
    # Peeled `^{}` line (annotated tag) wins over the tag object itself.
    tag_sha=$(awk -v r="$ref" '$2 == r "^{}" { print $1; f = 1; exit } $2 == r { s = $1 } END { if (!f && s) print s }' <<<"$tags")
    if [ -z "$tag_sha" ]; then
      echo "::error::missing release tag $ref" >&2
      exit 1
    fi
    if [ -n "$sha" ] && [ "$tag_sha" != "$sha" ]; then
      echo "::error::release tags of $registry_version disagree ($sha vs $tag_sha for $ref)" >&2
      exit 1
    fi
    sha=$tag_sha
  done
else
  # The commit the published tarballs were packaged from; they must agree.
  for crate in "${!crate_vcs[@]}"; do
    if [ -n "$sha" ] && [ "${crate_vcs[$crate]}" != "$sha" ]; then
      echo "::error::the published crates were packaged from different commits ($sha vs ${crate_vcs[$crate]} for $crate)" >&2
      exit 1
    fi
    sha=${crate_vcs[$crate]}
  done
  if [ -z "$sha" ]; then
    echo "::notice::no published crate to read the released commit from; using HEAD"
    sha=$head_sha
  fi
fi
# Consistency: every collected crate was packaged from the released commit.
for crate in "${!crate_vcs[@]}"; do
  if [ "${crate_vcs[$crate]}" != "$sha" ]; then
    echo "::error::$crate-${crate_version[$crate]}.crate was packaged from ${crate_vcs[$crate]}, not the released $sha" >&2
    exit 1
  fi
done

# Always a separate worktree, even when the released commit is HEAD: resolving
# the dependency graph rewrites a stale Cargo.lock, and the checkout must stay
# clean for the release job's next step (Cargo.lock sync on the version PR).
git -C "$root" cat-file -e "$sha^{commit}" 2>/dev/null ||
  git -C "$root" fetch --no-tags --depth=1 origin "$sha"
src=$(mktemp -d)
bridge_published_sbom=$(mktemp -d)
git -C "$root" worktree add --detach "$src" "$sha"
echo "released commit: $sha"

# The bridge SBOM that went to Maven Central (classifier `cyclonedx`) was built
# by the publish task; keep that exact file rather than a regenerated one
# (fresh serial number). Only when the publish ran on the released commit.
if [ -z "$registry_version" ]; then
  if [ "$sha" = "$head_sha" ]; then
    cp "$root"/libs/vespera-bridge/build/reports/cyclonedx-direct/*-cyclonedx.json \
      "$bridge_published_sbom/" 2>/dev/null || true
  fi
else
  # The copy on Maven Central, if actions/attest can take it (it needs a
  # `serialNumber`); otherwise the regenerated one is attested.
  f="$bridge_published_sbom/vespera-bridge-$registry_version-cyclonedx.json"
  if ! curl_get "https://repo1.maven.org/maven2/kr/devfive/vespera-bridge/$registry_version/$(basename "$f")" "$f" ||
    ! jq -e --arg v "$registry_version" '.bomFormat == "CycloneDX" and
      (.serialNumber | type == "string" and test("^urn:uuid:[0-9a-f-]{36}$")) and
      .metadata.component.version == $v' "$f" >/dev/null; then
    echo "::warning::not using the Maven Central vespera-bridge SBOM; attesting a regenerated one"
    rm -f "$f"
  fi
fi

# scripts/sbom.sh keeps going past a package it cannot describe; a package
# whose SBOM is missing is skipped below, the others are still collected.
if ! bash "$root/scripts/sbom.sh" "$out/sbom-all" "$src"; then
  fail "scripts/sbom.sh failed for at least one package (see above); packages without an SBOM are not attested"
fi
metadata=$(cargo metadata --manifest-path "$src/Cargo.toml" --no-deps --format-version 1)

record() { # <path> <version> <id> <artifact> <sbom>
  if [ ! -f "$5" ]; then
    fail "no SBOM $(basename "$5") for $3@$2; not attesting it"
    return 0
  fi
  if ! cp "$4" "$out/artifacts/" || ! cp "$5" "$out/sbom/"; then
    fail "could not copy the files of $3@$2; not attesting it"
    rm -f "$out/artifacts/$(basename "$4")" "$out/sbom/$(basename "$5")"
    return 0
  fi
  printf '%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "artifacts/$(basename "$4")" \
    "sbom/$(basename "$5")" >>"$out/manifest.tsv"
  collected=$((collected + 1))
}

for p in "${paths[@]}"; do
  case $p in
    crates/*/Cargo.toml)
      crate=${p#crates/}; crate=${crate%/Cargo.toml}
      # Skipped (and reported) above when it could not be verified.
      [ -n "${crate_file[$crate]:-}" ] || continue
      version=$(jq -r --arg n "$crate" '.packages[] | select(.name == $n) | .version' <<<"$metadata" | tr -d '\r')
      if [ "$version" != "${crate_version[$crate]}" ]; then
        fail "$crate: the released commit has version '$version', the published tarball ${crate_version[$crate]}; not attesting it"
        continue
      fi
      record "$p" "$version" "$crate" "${crate_file[$crate]}" "$out/sbom-all/$crate-$version.cdx.json"
      ;;
    libs/*/build.gradle.kts)
      lib=${p#libs/}; lib=${lib%/build.gradle.kts}
      version=$( { tr -d '\r' <"$src/$p" | sed -nE 's/^version = "([^"]+)"$/\1/p'; } 2>/dev/null) || version=
      if [ -z "$version" ]; then
        fail "could not read the version of $lib at the released commit; not attesting it"
        continue
      fi
      libs_dir="$root/libs/$lib/build/libs"
      if [ -n "$registry_version" ]; then
        libs_dir=$downloads
        if [ "$version" != "$registry_version" ] || [ ! -f "$libs_dir/$lib-$version.jar" ]; then
          fail "$lib@$registry_version: not downloaded, or the tagged commit has version $version; not attesting it"
          continue
        fi
      elif [ "$sha" != "$head_sha" ] || [ ! -f "$libs_dir/$lib-$version.jar" ]; then
        # Gradle 9 builds reproducible archives by default, so a rebuild of
        # the released commit should match what was uploaded (NOT verified
        # against Maven Central, unlike the crates).
        echo "::warning::$lib-$version.jar of the released commit not left by the publish task; rebuilding"
        libs_dir="$src/libs/$lib/build/libs"
        if ! (cd "$src/libs/$lib" && sh ./gradlew publishToMavenLocal --console=plain --no-daemon -q) ||
          [ ! -f "$libs_dir/$lib-$version.jar" ]; then
          fail "could not rebuild $lib-$version.jar; not attesting it"
          continue
        fi
      fi
      sbom="$out/sbom-all/$lib-$version-cyclonedx.json"
      if [ -f "$bridge_published_sbom/$lib-$version-cyclonedx.json" ]; then
        sbom="$bridge_published_sbom/$lib-$version-cyclonedx.json"
      fi
      before=$collected
      record "$p" "$version" "${lib//-/_}" "$libs_dir/$lib-$version.jar" "$sbom"
      if [ "$collected" -gt "$before" ]; then
        for jar in "$libs_dir/$lib-$version"{-sources,-javadoc}.jar; do
          if [ -f "$jar" ]; then cp "$jar" "$out/artifacts/" || fail "could not copy $(basename "$jar")"; fi
        done
      fi
      ;;
    *)
      echo "::warning::unknown published path $p; not attested"
      ;;
  esac
done
rm -rf "$out/sbom-all"

echo "── release artifacts ($collected of ${#paths[@]} packages collected)"
(cd "$out" && sha256sum artifacts/* sbom/* 2>/dev/null) || true
exit "$failed"
