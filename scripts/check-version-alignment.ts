// Every published package (the five `vespera*` crates, `vespera-bridge` and
// the Gradle plugin) ships under ONE version number. changepacks has no
// "fixed group" mode, so alignment holds only if
//   1. every package currently has the same version, and
//   2. the pending `.changepacks/changepack_log_*.json` entries give every
//      package the same highest bump level (none / Patch / Minor / Major).
// Run with `bun scripts/check-version-alignment.ts` (CI: lint job).
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

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
const LEVELS = ['none', 'Patch', 'Minor', 'Major'] as const

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
  versions.set(gradle, match(gradle, read(gradle), /^version\s*=\s*"([^"]+)"/m))
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
    const rank = LEVELS.indexOf(level as (typeof LEVELS)[number])
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

if (errors.length > 0) {
  console.error(errors.join('\n'))
  process.exit(1)
}
console.info(
  `all ${PACKAGES.length} packages at ${workspaceVersion}, pending bump: ${LEVELS[highest.values().next().value ?? 0]}`,
)
