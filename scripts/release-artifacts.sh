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

usage() { sed -n '2,39p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }
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

mapfile -t paths < <(jq -r 'if type == "array" then .[] else empty end' <<<"${published:-[]}" | tr -d '\r')
if [ "${#paths[@]}" -eq 0 ]; then
  echo "nothing was published in this run"
  output any=false
  exit 0
fi
printf 'published: %s\n' "${paths[@]}"

curl_get() { # <url> <file>: download with retries (registries can lag a publish)
  curl -fsSL --retry 5 --retry-delay 10 -A 'vespera-release (https://github.com/dev-five-git/vespera)' \
    -o "$2" "$1"
}

# ── --from-registries: the published files ────────────────────────────────
downloads=
if [ -n "$registry_version" ]; then
  downloads=$(mktemp -d)
  for p in "${paths[@]}"; do
    case $p in
      crates/*/Cargo.toml)
        crate=${p#crates/}; crate=${crate%/Cargo.toml}
        curl_get "https://static.crates.io/crates/$crate/$crate-$registry_version.crate" \
          "$downloads/$crate-$registry_version.crate" ||
          echo "::error::could not download $crate@$registry_version from crates.io"
        ;;
      libs/*/build.gradle.kts)
        lib=${p#libs/}; lib=${lib%/build.gradle.kts}
        repo=https://repo1.maven.org/maven2
        if [ "$lib" = vespera-bridge-gradle-plugin ]; then repo=https://plugins.gradle.org/m2; fi
        base="$repo/kr/devfive/$lib/$registry_version/$lib-$registry_version"
        for suffix in .jar -sources.jar -javadoc.jar; do
          f="$downloads/$lib-$registry_version$suffix"
          if curl_get "$base$suffix" "$f" && curl_get "$base$suffix.sha1" "$f.sha1" &&
            [ "$(sha1sum "$f" | cut -d' ' -f1)" = "$(tr -d ' \r\n' <"$f.sha1" | cut -c1-40)" ]; then
            rm "$f.sha1"
          else
            echo "::error::could not download (or SHA-1-verify) $lib-$registry_version$suffix"
            rm -f "$f" "$f.sha1"
          fi
        done
        ;;
    esac
  done
fi

# ── The released commit ───────────────────────────────────────────────────
sha=$(git -C "$root" rev-parse HEAD)
if [ -n "$registry_version" ]; then
  # Every changepacks tag of the version must exist and name the same commit.
  tags=$(git -C "$root" ls-remote --tags origin)
  sha=
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
  for crate_file in "$downloads"/*.crate; do
    [ -f "$crate_file" ] || continue
    vcs=$(tar -xzOf "$crate_file" --wildcards '*/.cargo_vcs_info.json' 2>/dev/null |
      jq -r '.git.sha1 // empty' || true)
    if [ -n "$vcs" ] && [ "$vcs" != "$sha" ]; then
      echo "::error::$(basename "$crate_file") was packaged from $vcs, not the tagged $sha" >&2
      exit 1
    fi
  done
fi
for p in "${paths[@]}"; do
  [ -z "$registry_version" ] || break
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
trap 'git -C "$root" worktree remove --force "$src" || true; rm -rf "$src" "$bridge_published_sbom" ${downloads:+"$downloads"} || true' EXIT
git -C "$root" worktree add --detach "$src" "$sha"
echo "released commit: $sha"

# The bridge SBOM that went to Maven Central (classifier `cyclonedx`) was built
# by the publish task; keep that exact file rather than a regenerated one
# (fresh serial number).
if [ -z "$registry_version" ]; then
  cp "$root"/libs/vespera-bridge/build/reports/cyclonedx-direct/*-cyclonedx.json \
    "$bridge_published_sbom/" 2>/dev/null || true
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
      if [ -n "$registry_version" ]; then
        file="$downloads/$crate-$version.crate"
        if [ "$version" != "$registry_version" ] || [ ! -f "$file" ]; then
          fail "$crate@$registry_version: not downloaded, or the tagged commit has version $version; not attesting it"
          continue
        fi
      elif [ ! -f "$file" ]; then
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
      if [ -n "$registry_version" ]; then
        libs_dir=$downloads
        if [ "$version" != "$registry_version" ] || [ ! -f "$libs_dir/$lib-$version.jar" ]; then
          fail "$lib@$registry_version: not downloaded, or the tagged commit has version $version; not attesting it"
          continue
        fi
      elif [ ! -f "$libs_dir/$lib-$version.jar" ]; then
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
