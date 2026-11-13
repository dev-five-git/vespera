#!/usr/bin/env bash
# Generate (and sanity-check) the CycloneDX SBOMs of every published package.
#
#   scripts/sbom.sh <out-dir> [<repo-root>]
#
# Writes into <out-dir>:
#   <crate>-<version>.cdx.json                       one per crates.io crate
#                                                    (cargo-cyclonedx, all features,
#                                                    all targets, no dev-deps)
#   vespera-bridge-<version>-cyclonedx.json          Maven Central `cyclonedx` classifier
#   vespera-bridge-gradle-plugin-<version>-cyclonedx.json
#
# Runs on every PR (CI `sbom` job) and, from the released commit, in the main-only
# release job, which attests each SBOM against the artifact it describes.
#
# Needs: cargo, jq, a JDK for the Gradle wrappers, and cargo-cyclonedx — taken
# from $CARGO_CYCLONEDX, else `cargo cyclonedx` on PATH, else (Linux x86_64 only)
# the pinned release binary below, verified against its SHA-256 before use.
set -euo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
fi

# <repo-root> (default: this checkout) lets the release job describe the released
# commit from a separate worktree.
root=$(cd "${2:-$(dirname "$0")/..}" && pwd)
mkdir -p "$1"
out=$(cd "$1" && pwd)

crates=(vespera vespera_core vespera_macro vespera_inprocess vespera_jni)

# Pinned cargo-cyclonedx (bump the version and hash together; the hash is the
# one in taiki-e/install-action's manifest and on the GitHub release).
CDX_VERSION=0.5.9
CDX_SHA256=9bd3e599314f50810c9d98b8b68a617ff9d3cc20873968d90b29d121f6b226ff
CDX_URL="https://github.com/CycloneDX/cyclonedx-rust-cargo/releases/download/cargo-cyclonedx-$CDX_VERSION/cargo-cyclonedx-x86_64-unknown-linux-musl.tar.xz"

cdx=()
if [ -n "${CARGO_CYCLONEDX:-}" ]; then
  cdx=("$CARGO_CYCLONEDX" cyclonedx)
elif cargo cyclonedx --version >/dev/null 2>&1; then
  cdx=(cargo cyclonedx)
elif [ "$(uname -s)-$(uname -m)" = Linux-x86_64 ]; then
  tool_dir=$(mktemp -d)
  trap 'rm -rf "$tool_dir"' EXIT
  curl -fsSL --retry 3 -o "$tool_dir/cdx.tar.xz" "$CDX_URL"
  echo "$CDX_SHA256  $tool_dir/cdx.tar.xz" | sha256sum -c --quiet -
  tar -xJf "$tool_dir/cdx.tar.xz" -C "$tool_dir"
  cdx=("$tool_dir/cargo-cyclonedx-x86_64-unknown-linux-musl/cargo-cyclonedx" cyclonedx)
else
  echo "cargo-cyclonedx not found: install it or set CARGO_CYCLONEDX" >&2
  exit 1
fi

# Assert an SBOM is CycloneDX JSON describing <name>@<version>. actions/attest
# rejects a CycloneDX document without a `serialNumber` ("Unsupported SBOM
# format"), so that is checked here too, on every PR.
check_sbom() { # <file> <name> <version>
  jq -e --arg n "$2" --arg v "$3" '
    .bomFormat == "CycloneDX"
    and (.specVersion | type == "string")
    and (.serialNumber | type == "string" and test("^urn:uuid:[0-9a-f-]{36}$"))
    and .metadata.component.name == $n
    and .metadata.component.version == $v
  ' "$1" >/dev/null || {
    echo "::error::$1 is not a CycloneDX SBOM for $2@$3" >&2
    exit 1
  }
  echo "ok  $(basename "$1")  ($(jq '.components // [] | length' "$1") components)"
}

# ── Rust crates ────────────────────────────────────────────────────────────
# Reproducible timestamps: the SBOM of a commit is dated by the commit.
SOURCE_DATE_EPOCH=$(git -C "$root" log -1 --format=%ct 2>/dev/null || date +%s)
export SOURCE_DATE_EPOCH
commit=$(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)

# cargo-cyclonedx writes no `serialNumber`. Add one that stays reproducible: a
# name-based (version 5 layout) UUID of <name>@<version>@<commit>, placed after
# `specVersion` like the Gradle plugin's.
add_serial_number() { # <file> <name> <version>
  local h uuid
  h=$(printf '%s@%s@%s' "$2" "$3" "$commit" | sha1sum | cut -c1-32)
  uuid="${h:0:8}-${h:8:4}-5${h:13:3}-$(printf '%x' $(( (16#${h:16:1} & 3) | 8 )))${h:17:3}-${h:20:12}"
  jq --arg s "urn:uuid:$uuid"     '{bomFormat, specVersion, serialNumber: $s} + del(.serialNumber)' "$1" >"$1.tmp"
  mv "$1.tmp" "$1"
}
metadata=$(cargo metadata --manifest-path "$root/Cargo.toml" --no-deps --format-version 1)
# cargo-cyclonedx always describes every workspace member, next to its manifest.
"${cdx[@]}" --manifest-path "$root/Cargo.toml" --format json \
  --all-features --target all --spec-version 1.5 -q
for crate in "${crates[@]}"; do
  version=$(jq -r --arg n "$crate" '.packages[] | select(.name == $n) | .version' <<<"$metadata" | tr -d '\r')
  dest="$out/$crate-$version.cdx.json"
  mv "$root/crates/$crate/$crate.cdx.json" "$dest"
  add_serial_number "$dest" "$crate" "$version"
  check_sbom "$dest" "$crate" "$version"
done
# Drop the SBOMs of the unpublished members (examples, benches).
jq -r '.packages[] | (.manifest_path | rtrimstr("Cargo.toml")) + .name + ".cdx.json"' <<<"$metadata" |
  while IFS= read -r f; do rm -f "${f%$'\r'}"; done

# ── Java libraries ─────────────────────────────────────────────────────────
for lib in vespera-bridge vespera-bridge-gradle-plugin; do
  dir="$root/libs/$lib"
  version=$(tr -d '\r' <"$dir/build.gradle.kts" | sed -nE 's/^version = "([^"]+)"$/\1/p')
  (cd "$dir" && sh ./gradlew cyclonedxDirectBom --console=plain --no-daemon -q)
  src="$dir/build/reports/cyclonedx-direct/$lib-$version-cyclonedx.json"
  cp "$src" "$out/"
  check_sbom "$out/$(basename "$src")" "$lib" "$version"
done
