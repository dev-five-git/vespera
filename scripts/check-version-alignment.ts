// Every published package (the five `vespera*` crates, `vespera-bridge` and
// the Gradle plugin) ships under ONE version number. changepacks has no
// "fixed group" mode, so alignment holds only if
//   1. every package currently has the same version, and
//   2. the pending `.changepacks/changepack_log_*.json` entries give every
//      package the same highest bump level (none / Patch / Minor / Major).
// It also checks
//   3. the versions the landing documentation quotes
//      (`apps/landing/src/constants/index.ts` — the single source the MDX code
//      samples are tested against in `docs-versions.test.ts`), and
//   4. the versions the other user-facing docs quote (`REPO_DOCS`: the root
//      README is every crate's crates.io page, `crates/vespera/src/lib.rs`
//      the docs.rs front page, the bridge README the Maven Central one).
//
// Rule for 3 and 4: compared at `major.minor` level only (the docs say
// `vespera = "0.6"` and `vespera-bridge:0.6.0`, and the bridge is
// minor-compatible with the crate), and a documented version passes when it
// matches EITHER the current workspace version OR the version the pending
// changepacks will release (current version + the pending bump level).
// Accepting both means a PR that adds the first Minor changepack does not
// have to rewrite the docs immediately, while docs that drift to any other
// line (stale `0.4`, typo'd `0.7` before a Minor bump) fail CI.
//
// Run with `bun scripts/check-version-alignment.ts` (CI: lint job). The pure
// helpers are exported for `check-version-alignment.test.ts`.
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

import * as landing from '../apps/landing/src/constants'

export const LEVELS = ['none', 'Patch', 'Minor', 'Major'] as const

type DocConstant = (typeof landing.DOC_VERSION_PATTERNS)[number][1]

/**
 * Docs outside the landing site whose code samples quote package versions.
 * `sync-landing-versions.ts` rewrites them on the version PR like the landing
 * pages; the published `.crate` / JAR READMEs can never be replaced, so a
 * stale pin here ships for good.
 */
export const REPO_DOCS = [
  'README.md',
  'SKILL.md',
  'crates/vespera/src/lib.rs',
  'examples/rust-jni-demo/README.md',
  'libs/vespera-bridge/README.md',
]

/**
 * `DOC_VERSION_PATTERNS` for `REPO_DOCS`: the crate pins are not anchored to
 * the line start (only to a non-name character before `vespera`), since
 * these files also quote them in comments
 * (`//! vespera = "0.8"`, `// Cargo.toml: vespera = { version = ... }`).
 */
export const REPO_VERSION_PATTERNS: [RegExp, DocConstant][] = [
  [/(?<![\w-])vespera = "([0-9][^"]*)"/g, 'VESPERA_VERSION'],
  [/(?<![\w-])vespera = \{ version = "([0-9][^"]*)"/g, 'VESPERA_VERSION'],
  ...landing.DOC_VERSION_PATTERNS.filter(([, c]) => c !== 'VESPERA_VERSION'),
]

/** Every `REPO_VERSION_PATTERNS` capture in `content`, with its line. */
export function repoDocVersions(
  content: string,
): { line: number; version: string }[] {
  const found: { line: number; version: string; index: number }[] = []
  for (const [pattern] of REPO_VERSION_PATTERNS) {
    for (const match of content.matchAll(pattern)) {
      const index = match.index ?? 0
      found.push({
        line: content.slice(0, index).split('\n').length,
        version: match[1],
        index,
      })
    }
  }
  return found
    .sort((a, b) => a.index - b.index)
    .map(({ line, version }) => ({ line, version }))
}
export type Level = (typeof LEVELS)[number]

/** `major.minor` of a version string (`"0.6"` / `"0.6.0"` / `"0.6.0-rc.1"`). */
export function minorOf(version: string): string | null {
  const m = /^(\d+)\.(\d+)(?:\.\d+)?(?:[-+].*)?$/.exec(version.trim())
  return m ? `${Number(m[1])}.${Number(m[2])}` : null
}

/** Semver bump of `version` by `level` (`none` returns it unchanged). */
export function bumpVersion(version: string, level: Level): string {
  const m = /^(\d+)\.(\d+)\.(\d+)/.exec(version)
  if (!m) throw new Error(`not a semver version: ${version}`)
  const [major, minor, patch] = [Number(m[1]), Number(m[2]), Number(m[3])]
  switch (level) {
    case 'Major':
      return `${major + 1}.0.0`
    case 'Minor':
      return `${major}.${minor + 1}.0`
    case 'Patch':
      return `${major}.${minor}.${patch + 1}`
    default:
      return version
  }
}

/**
 * Errors for documented versions that match neither the current workspace
 * version nor the pending release (both at `major.minor` level).
 */
export function checkDocumentedVersions(
  documented: Record<string, string>,
  current: string,
  pending: Level,
  where = 'landing',
  fix = 'update apps/landing/src/constants/index.ts and the MDX samples ' +
    '(docs-versions.test.ts lists them)',
): string[] {
  const next = bumpVersion(current, pending)
  const accepted = new Set([minorOf(current), minorOf(next)])
  const allowed = [...accepted].join(' or ')
  const errors: string[] = []
  for (const [name, version] of Object.entries(documented)) {
    const minor = minorOf(version)
    if (minor === null) {
      errors.push(`${where} ${name} = "${version}" is not a version`)
    } else if (!accepted.has(minor)) {
      errors.push(
        `${where} ${name} = "${version}" (${minor}) does not match the ` +
          `workspace ${current}` +
          (next === current ? '' : ` / pending release ${next}`) +
          ` — expected ${allowed}; ${fix}`,
      )
    }
  }
  return errors
}

function main(): number {
  const root = join(import.meta.dir, '..')
  const read = (path: string) => readFileSync(join(root, path), 'utf8')

  const CRATES = [
    'crates/vespera/Cargo.toml',
    'crates/vespera_core/Cargo.toml',
    'crates/vespera_macro/Cargo.toml',
    'crates/vespera_inprocess/Cargo.toml',
    'crates/vespera_jni/Cargo.toml',
  ]
  const GRADLE = [
    'libs/vespera-bridge/build.gradle.kts',
    'libs/vespera-bridge-gradle-plugin/build.gradle.kts',
  ]
  const PACKAGES = [...CRATES, ...GRADLE]

  const errors: string[] = []

  function match(path: string, source: string, pattern: RegExp): string {
    const found = source.match(pattern)?.[1]
    if (!found) errors.push(`${path}: no version found`)
    return found ?? '?'
  }

  // 1. current versions
  const workspaceVersion = match(
    'Cargo.toml',
    read('Cargo.toml'),
    /\[workspace\.package\][^[]*?\nversion\s*=\s*"([^"]+)"/,
  )
  const versions = new Map<string, string>()
  for (const crate of CRATES) {
    versions.set(
      crate,
      /^version\.workspace\s*=\s*true/m.test(read(crate))
        ? workspaceVersion
        : match(crate, read(crate), /^version\s*=\s*"([^"]+)"/m),
    )
  }
  for (const gradle of GRADLE) {
    versions.set(
      gradle,
      match(gradle, read(gradle), /^version\s*=\s*"([^"]+)"/m),
    )
  }
  if (new Set(versions.values()).size !== 1) {
    errors.push(
      'packages have different versions:\n' +
        [...versions].map(([p, v]) => `  ${v}  ${p}`).join('\n'),
    )
  }

  // 2. pending bump levels
  const highest = new Map<string, number>(PACKAGES.map((p) => [p, 0]))
  const dir = join(root, '.changepacks')
  for (const file of readdirSync(dir).filter((f) =>
    f.startsWith('changepack_log_'),
  )) {
    const changes: Record<string, string> =
      JSON.parse(readFileSync(join(dir, file), 'utf8')).changes ?? {}
    for (const [path, level] of Object.entries(changes)) {
      const rank = LEVELS.indexOf(level as Level)
      if (!highest.has(path) || rank < 0) {
        errors.push(`${file}: unknown package or level ${path} = ${level}`)
        continue
      }
      highest.set(path, Math.max(highest.get(path) ?? 0, rank))
    }
  }
  if (new Set(highest.values()).size !== 1) {
    errors.push(
      'pending changepacks bump packages by different levels (list every ' +
        'package in .changepacks with the same level):\n' +
        [...highest].map(([p, r]) => `  ${LEVELS[r]}  ${p}`).join('\n'),
    )
  }
  const pending = LEVELS[highest.values().next().value ?? 0]

  // 3. versions quoted by the landing documentation
  const documented = {
    VESPERA_VERSION: landing.VESPERA_VERSION,
    VESPERA_BRIDGE_VERSION: landing.VESPERA_BRIDGE_VERSION,
    VESPERA_BRIDGE_PLUGIN_VERSION: landing.VESPERA_BRIDGE_PLUGIN_VERSION,
  }
  if (workspaceVersion !== '?') {
    errors.push(
      ...checkDocumentedVersions(documented, workspaceVersion, pending),
    )
    // 4. versions quoted by the other user-facing docs
    for (const file of REPO_DOCS) {
      for (const { line, version } of repoDocVersions(read(file))) {
        errors.push(
          ...checkDocumentedVersions(
            { [`${file}:${line}`]: version },
            workspaceVersion,
            pending,
            'quoted',
            'run `bun scripts/sync-landing-versions.ts`',
          ),
        )
      }
    }
  }

  if (errors.length > 0) {
    console.error(errors.join('\n'))
    return 1
  }
  console.info(
    `all ${PACKAGES.length} packages at ${workspaceVersion}, pending bump: ` +
      `${pending}; landing documents ${Object.values(documented).join(' / ')}` +
      `; ${REPO_DOCS.length} other docs checked`,
  )
  return 0
}

if (import.meta.main) process.exit(main())
