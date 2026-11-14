// Rewrite the versions the documentation quotes to the workspace version:
// the constants in `apps/landing/src/constants/index.ts`, every MDX
// code-sample literal `DOC_VERSION_PATTERNS` matches, and the samples in the
// other user-facing docs (`REPO_DOCS` / `REPO_VERSION_PATTERNS` from
// `check-version-alignment.ts` — the READMEs that become the crates.io,
// docs.rs and Maven Central pages). changepacks bumps
// only the package manifests, so the release job runs this on the version PR
// (next to the Cargo.lock sync); without it `check-version-alignment.ts`
// fails main's lint job right after the version PR merges and the release
// never runs (0.7.0).
//
// Run with `bun scripts/sync-landing-versions.ts` from the repository root.
// The pure helpers are exported for `sync-landing-versions.test.ts`.
import { readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

import { DOC_VERSION_PATTERNS } from '../apps/landing/src/constants'
import {
  minorOf,
  REPO_DOCS,
  REPO_VERSION_PATTERNS,
} from './check-version-alignment'

export interface DocVersions {
  VESPERA_VERSION: string
  VESPERA_BRIDGE_VERSION: string
  VESPERA_BRIDGE_PLUGIN_VERSION: string
}

/** The documented versions for workspace version `version`. */
export function docVersionsFor(version: string): DocVersions {
  const minor = minorOf(version)
  if (minor === null) throw new Error(`not a version: ${version}`)
  return {
    VESPERA_VERSION: minor,
    VESPERA_BRIDGE_VERSION: version,
    VESPERA_BRIDGE_PLUGIN_VERSION: version,
  }
}

/** The `export const NAME = '...'` version lines, rewritten. */
export function rewriteConstants(
  source: string,
  versions: DocVersions,
): string {
  let out = source
  for (const [name, version] of Object.entries(versions)) {
    out = out.replace(
      new RegExp(`^(export const ${name} = ')[^']*(')`, 'm'),
      `$1${version}$2`,
    )
  }
  return out
}

/** Every `patterns` capture (default: an MDX page's), rewritten. */
export function rewriteDocSamples(
  content: string,
  versions: DocVersions,
  patterns: [RegExp, keyof DocVersions][] = DOC_VERSION_PATTERNS,
): string {
  let out = content
  for (const [pattern, constant] of patterns) {
    out = out.replace(pattern, (match: string, found: string) =>
      match.replace(found, versions[constant]),
    )
  }
  return out
}

function main(): number {
  const root = join(import.meta.dir, '..')
  const workspace = readFileSync(join(root, 'Cargo.toml'), 'utf8').match(
    /\[workspace\.package\][^[]*?\nversion\s*=\s*"([^"]+)"/,
  )?.[1]
  if (!workspace) {
    console.error('Cargo.toml: no [workspace.package] version')
    return 1
  }
  const versions = docVersionsFor(workspace)
  const changed: string[] = []
  const rewrite = (path: string, f: (source: string) => string) => {
    const before = readFileSync(join(root, path), 'utf8')
    const after = f(before)
    if (after !== before) {
      writeFileSync(join(root, path), after)
      changed.push(path)
    }
  }
  rewrite('apps/landing/src/constants/index.ts', (s) =>
    rewriteConstants(s, versions),
  )
  const docs = 'apps/landing/src/app/documentation/[...name]'
  for (const file of readdirSync(join(root, docs))) {
    if (file.endsWith('.mdx')) {
      rewrite(`${docs}/${file}`, (s) => rewriteDocSamples(s, versions))
    }
  }
  for (const file of REPO_DOCS) {
    rewrite(file, (s) => rewriteDocSamples(s, versions, REPO_VERSION_PATTERNS))
  }
  console.info(
    `docs quote ${Object.values(versions).join(' / ')}; ` +
      (changed.length > 0
        ? `rewrote ${changed.join(', ')}`
        : 'already in sync'),
  )
  return 0
}

if (import.meta.main) process.exit(main())
