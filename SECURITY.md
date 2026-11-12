# Security Policy

## Reporting a vulnerability

Please **do not** open a public issue, discussion or pull request for a
security problem.

- **Preferred:** report it privately through GitHub —
  [Security → Report a vulnerability](https://github.com/dev-five-git/vespera/security/advisories/new)
  (private vulnerability reporting is enabled for this repository).
- **Alternatively:** email <contact@devfive.kr> with "vespera security" in the
  subject.

Include the affected package(s) and version(s), a description of the impact, and
a reproduction (a minimal route / wire request / Java snippet is ideal). We will
acknowledge the report, keep you informed while we work on a fix, and credit you
in the advisory unless you prefer otherwise. Please give us a reasonable time to
release a fix before disclosing the issue publicly.

## Supported versions

Vespera is pre-1.0. All seven packages (the five crates, `kr.devfive:vespera-bridge`
and the `kr.devfive.vespera-bridge` Gradle plugin) are released together under one
version, and security fixes land in the **latest released version** only.

## Verifying releases

Every release is built and published by the `changepacks` job of
[`.github/workflows/CI.yml`](.github/workflows/CI.yml) on `main`, only after all CI
gates pass. For releases published after 0.5.0 that job also records
[GitHub artifact attestations](https://docs.github.com/actions/security-for-github-actions/using-artifact-attestations)
(signed with Sigstore) for exactly the files it published:

| Package | Attested files | SBOM (CycloneDX JSON) |
|---|---|---|
| crates `vespera`, `vespera_core`, `vespera_macro`, `vespera_inprocess`, `vespera_jni` | the `.crate` uploaded to crates.io | GitHub release asset `<crate>-<version>.cdx.json` |
| `kr.devfive:vespera-bridge` | main, `-sources`, `-javadoc` JARs | Maven Central classifier `cyclonedx` (`vespera-bridge-<version>-cyclonedx.json`) and GitHub release asset |
| `kr.devfive:vespera-bridge-gradle-plugin` | main, `-sources`, `-javadoc` JARs | GitHub release asset `vespera-bridge-gradle-plugin-<version>-cyclonedx.json` |

Two kinds of attestation exist for each package:

- **Build provenance** (SLSA v1, the default predicate) for every published file,
  the SBOMs included: which repository, workflow, commit and run produced it.
- **SBOM attestation** (predicate `https://cyclonedx.org/bom`) binding each
  package's artifact to the CycloneDX SBOM that describes it.

Verify with the [GitHub CLI](https://cli.github.com/) (`gh attestation verify`
checks the Sigstore signature, the certificate identity and the artifact digest):

```bash
V=0.6.0   # the version you depend on

# A crate, exactly as crates.io serves it
curl -fsSLO "https://static.crates.io/crates/vespera/vespera-$V.crate"
gh attestation verify "vespera-$V.crate" --repo dev-five-git/vespera \
  --signer-workflow dev-five-git/vespera/.github/workflows/CI.yml \
  --source-ref refs/heads/main

# The Java bridge from Maven Central (same for vespera-bridge-gradle-plugin)
curl -fsSLO "https://repo1.maven.org/maven2/kr/devfive/vespera-bridge/$V/vespera-bridge-$V.jar"
gh attestation verify "vespera-bridge-$V.jar" --repo dev-five-git/vespera

# Its SBOM attestation, printing the attested CycloneDX document
gh attestation verify "vespera-bridge-$V.jar" --repo dev-five-git/vespera \
  --predicate-type https://cyclonedx.org/bom \
  --format json --jq '.[0].verificationResult.statement.predicate'

# A downloaded SBOM file is itself covered by build provenance
curl -fsSLO "https://repo1.maven.org/maven2/kr/devfive/vespera-bridge/$V/vespera-bridge-$V-cyclonedx.json"
gh attestation verify "vespera-bridge-$V-cyclonedx.json" --repo dev-five-git/vespera
```

`gh attestation verify` exits non-zero when no valid attestation matches the
file's SHA-256, so it can gate a CI pipeline. Maven Central artifacts are also
GPG-signed (`.asc`), as Central requires.

Notes:

- The crate SBOMs are generated with `cargo-cyclonedx` for **all features and all
  targets** (a superset of any single build) and exclude dev-dependencies. The
  Java SBOMs list the published JAR's runtime dependencies — currently none: the
  bridge's Spring / servlet / Micrometer APIs are `compileOnly` and provided by
  your application.
- The same SBOMs are generated on every pull request (CI `sbom` job, artifact
  `sbom`) via [`scripts/sbom.sh`](scripts/sbom.sh).
