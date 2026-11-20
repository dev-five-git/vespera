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
import {
  lstatSync,
  readdirSync,
  readFileSync,
  realpathSync,
  writeFileSync,
} from 'node:fs'
import { join, sep } from 'node:path'

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
    out = replaceCapture(out, pattern, versions[constant])
  }
  return out
}

/**
 * Replace capture group 1 of every `pattern` match (the first only for a
 * non-global pattern) with `value` — at the capture's own position: the
 * captured text may also occur earlier in the match (`v1 = "1"`).
 */
function replaceCapture(
  content: string,
  pattern: RegExp,
  value: string,
): string {
  const flags = pattern.flags.includes('d')
    ? pattern.flags
    : `${pattern.flags}d`
  const regex = new RegExp(pattern.source, flags)
  const matches = regex.global
    ? [...content.matchAll(regex)]
    : [regex.exec(content)].filter((match) => match !== null)
  let out = ''
  let last = 0
  for (const match of matches) {
    const span = match.indices?.[1]
    if (!span) continue
    out += content.slice(last, span[0]) + value
    last = span[1]
  }
  return out + content.slice(last)
}

// The absolute path of a file the sync may rewrite under `root`. The checkout is
// the untrusted PR tree, so a symlink is refused rather than followed, and so is
// a path whose real location leaves the root: writeFileSync would otherwise
// overwrite whatever file the link names. Throws with the reason.
export function rewritableFile(root: string, path: string): string {
  const full = join(root, path)
  if (lstatSync(full).isSymbolicLink()) {
    throw new Error(`${path} is a symbolic link; refusing to rewrite it`)
  }
  const rootReal = realpathSync(root)
  const real = realpathSync(full)
  if (!real.startsWith(rootReal + sep)) {
    throw new Error(
      `${path} resolves outside the checkout; refusing to rewrite it`,
    )
  }
  return full
}

function main(): number {
  // VERSION_SYNC_ROOT lets CI run this file from a trusted copy against the
  // checkout it rewrites (see the changepacks job in .github/workflows/CI.yml).
  const root = process.env.VERSION_SYNC_ROOT ?? join(import.meta.dir, '..')
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
    const full = rewritableFile(root, path)
    const before = readFileSync(full, 'utf8')
    const after = f(before)
    if (after !== before) {
      writeFileSync(full, after)
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

if (import.meta.main) {
  try {
    process.exit(main())
  } catch (error) {
    // A refused symlink (or any unreadable path) fails the run instead of
    // leaving the docs half rewritten without a message.
    console.error(error instanceof Error ? error.message : error)
    process.exit(1)
  }
}
