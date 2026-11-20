import {
  mkdirSync,
  mkdtempSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { afterAll, describe, expect, it } from 'bun:test'

import {
  REPO_VERSION_PATTERNS,
  repoDocVersions,
} from './check-version-alignment'
import {
  docVersionsFor,
  rewritableFile,
  rewriteConstants,
  rewriteDocSamples,
} from './sync-landing-versions'

const V08 = docVersionsFor('0.8.0')

describe('docVersionsFor', () => {
  it('quotes the crate at major.minor and the Java packages in full', () => {
    expect(V08).toEqual({
      VESPERA_VERSION: '0.8',
      VESPERA_BRIDGE_VERSION: '0.8.0',
      VESPERA_BRIDGE_PLUGIN_VERSION: '0.8.0',
    })
  })

  it('rejects a non-version', () => {
    expect(() => docVersionsFor('latest')).toThrow()
  })
})

describe('rewriteConstants', () => {
  it('rewrites only the three version constants', () => {
    const source = [
      "export const VESPERA_VERSION = '0.7'",
      "export const VESPERA_BRIDGE_VERSION = '0.7.0'",
      "export const VESPERA_BRIDGE_PLUGIN_VERSION = '0.7.0'",
      "export const OTHER = '0.7.0'",
    ].join('\n')
    expect(rewriteConstants(source, V08)).toBe(
      [
        "export const VESPERA_VERSION = '0.8'",
        "export const VESPERA_BRIDGE_VERSION = '0.8.0'",
        "export const VESPERA_BRIDGE_PLUGIN_VERSION = '0.8.0'",
        "export const OTHER = '0.7.0'",
      ].join('\n'),
    )
  })
})

describe('rewriteDocSamples', () => {
  it.each([
    ['vespera = "0.7"', 'vespera = "0.8"'],
    [
      'vespera = { version = "0.7", features = ["jni"] }',
      'vespera = { version = "0.8", features = ["jni"] }',
    ],
    [
      'implementation("kr.devfive:vespera-bridge:0.7.0")',
      'implementation("kr.devfive:vespera-bridge:0.8.0")',
    ],
    [
      '<artifactId>vespera-bridge</artifactId>\n  <version>0.7.0</version>',
      '<artifactId>vespera-bridge</artifactId>\n  <version>0.8.0</version>',
    ],
    ['bridgeVersion.set("0.7.0")', 'bridgeVersion.set("0.8.0")'],
    [
      'id("kr.devfive.vespera-bridge") version "0.7.0"',
      'id("kr.devfive.vespera-bridge") version "0.8.0"',
    ],
  ])('%p', (before, after) => {
    expect(rewriteDocSamples(before, V08)).toBe(after)
  })

  it('leaves prose and other versions alone', () => {
    const prose = 'The 0.7 release adds X.\nserde = "1.0"\n'
    expect(rewriteDocSamples(prose, V08)).toBe(prose)
  })

  it('rewrites the capture at its own position, not an earlier copy', () => {
    const patterns: [RegExp, 'VESPERA_VERSION'][] = [
      [/v1 = "(\d)"/g, 'VESPERA_VERSION'],
    ]
    expect(rewriteDocSamples('v1 = "1"\nv1 = "1"', V08, patterns)).toBe(
      'v1 = "0.8"\nv1 = "0.8"',
    )
  })

  it('rewrites only the first match of a non-global pattern and skips an empty capture', () => {
    const patterns: [RegExp, 'VESPERA_VERSION'][] = [
      [/x = "(\d)"/, 'VESPERA_VERSION'],
      [/z(\d)?/g, 'VESPERA_VERSION'],
    ]
    expect(rewriteDocSamples('x = "1"\nx = "2"\nz', V08, patterns)).toBe(
      'x = "0.8"\nx = "2"\nz',
    )
  })
})

describe('rewriteDocSamples with REPO_VERSION_PATTERNS', () => {
  it.each([
    ['//! vespera = "0.5"', '//! vespera = "0.8"'],
    [
      '// Cargo.toml: vespera = { version = "0.7", features = ["ffi"] }',
      '// Cargo.toml: vespera = { version = "0.8", features = ["ffi"] }',
    ],
    [
      '  <artifactId>vespera-bridge</artifactId>\n  <version>0.6.0</version>',
      '  <artifactId>vespera-bridge</artifactId>\n  <version>0.8.0</version>',
    ],
    [
      '    id("kr.devfive.vespera-bridge") version "..."',
      '    id("kr.devfive.vespera-bridge") version "0.8.0"',
    ],
  ])('%p', (before, after) => {
    expect(rewriteDocSamples(before, V08, REPO_VERSION_PATTERNS)).toBe(after)
  })

  it('leaves other crates and placeholder crate pins alone', () => {
    const source =
      'vespera_core = "0.7"\nmy-vespera = "0.7"\n(bridge `X.Y.z` ↔ `vespera = "X.Y"`)\n'
    expect(rewriteDocSamples(source, V08, REPO_VERSION_PATTERNS)).toBe(source)
  })
})

describe('repoDocVersions', () => {
  it('lists every quoted version with its line, in file order', () => {
    const source = [
      '# Install',
      'vespera = { version = "0.7", features = ["jni"] }',
      'implementation("kr.devfive:vespera-bridge:0.7.0")',
      '//! vespera = "0.6"',
    ].join('\n')
    expect(repoDocVersions(source)).toEqual([
      { line: 2, version: '0.7' },
      { line: 3, version: '0.7.0' },
      { line: 4, version: '0.6' },
    ])
  })
})

describe('rewritableFile', () => {
  // The checkout is the untrusted PR tree: a symlink (or a link to a directory)
  // must never redirect the rewrite outside the root.
  const sandbox = mkdtempSync(join(tmpdir(), 'sync-landing-versions-'))
  const root = join(sandbox, 'root')
  const outside = join(sandbox, 'outside')
  mkdirSync(join(root, 'docs'), { recursive: true })
  mkdirSync(outside)
  writeFileSync(join(root, 'docs', 'page.mdx'), '')
  writeFileSync(join(outside, 'secret.mdx'), '')
  writeFileSync(join(outside, 'x.mdx'), '')
  // File symlinks need a privilege some Windows setups lack; directory
  // junctions do not. Each test is skipped where its own link kind fails.
  const canLink = (target: string, type?: 'junction', name = 'probe') => {
    try {
      symlinkSync(target, join(sandbox, name), type)
      return true
    } catch {
      return false
    }
  }
  const canLinkFile = canLink(
    join(outside, 'secret.mdx'),
    undefined,
    'probe-file',
  )
  const canLinkDir = canLink(outside, 'junction', 'probe-dir')

  it('accepts a regular file under the root', () => {
    expect(rewritableFile(root, 'docs/page.mdx')).toBe(
      join(root, 'docs', 'page.mdx'),
    )
  })

  it.skipIf(!canLinkFile)(
    'refuses a symlink to a file outside the root',
    () => {
      symlinkSync(join(outside, 'secret.mdx'), join(root, 'docs', 'link.mdx'))
      expect(() => rewritableFile(root, 'docs/link.mdx')).toThrow(
        'is a symbolic link',
      )
    },
  )

  it.skipIf(!canLinkDir)(
    'refuses a file reached through a linked directory',
    () => {
      symlinkSync(outside, join(root, 'linkdir'), 'junction')
      expect(() => rewritableFile(root, 'linkdir/x.mdx')).toThrow(
        'resolves outside the checkout',
      )
    },
  )

  afterAll(() => {
    rmSync(sandbox, { recursive: true, force: true })
  })
})
