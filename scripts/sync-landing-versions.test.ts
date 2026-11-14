import { describe, expect, it } from 'bun:test'

import {
  REPO_VERSION_PATTERNS,
  repoDocVersions,
} from './check-version-alignment'
import {
  docVersionsFor,
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
