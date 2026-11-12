import { describe, expect, it } from 'bun:test'

import {
  bumpVersion,
  checkDocumentedVersions,
  minorOf,
} from './check-version-alignment'

describe('minorOf', () => {
  it.each([
    ['0.6', '0.6'],
    ['0.6.0', '0.6'],
    ['0.6.12', '0.6'],
    ['1.10.3-rc.1', '1.10'],
    [' 0.6 ', '0.6'],
  ])('%s -> %s', (version, expected) => {
    expect(minorOf(version)).toBe(expected)
  })

  it.each(['', '0', 'latest', '0.x', 'v0.6'])('rejects %p', (version) => {
    expect(minorOf(version)).toBeNull()
  })
})

describe('bumpVersion', () => {
  it.each([
    ['0.5.0', 'none', '0.5.0'],
    ['0.5.3', 'Patch', '0.5.4'],
    ['0.5.3', 'Minor', '0.6.0'],
    ['0.5.3', 'Major', '1.0.0'],
  ] as const)('%s + %s -> %s', (version, level, expected) => {
    expect(bumpVersion(version, level)).toBe(expected)
  })
})

describe('checkDocumentedVersions', () => {
  const docs = (version: string, bridge = `${version}.0`) => ({
    VESPERA_VERSION: version,
    VESPERA_BRIDGE_VERSION: bridge,
  })

  it('accepts the pending release line', () => {
    expect(checkDocumentedVersions(docs('0.6'), '0.5.0', 'Minor')).toEqual([])
  })

  it('accepts the current release line while a bump is pending', () => {
    expect(checkDocumentedVersions(docs('0.5'), '0.5.0', 'Minor')).toEqual([])
  })

  it('accepts any patch of the documented minor', () => {
    expect(
      checkDocumentedVersions(docs('0.6', '0.6.0'), '0.6.4', 'Patch'),
    ).toEqual([])
  })

  it('rejects a stale minor', () => {
    const errors = checkDocumentedVersions(docs('0.4'), '0.5.0', 'Minor')
    expect(errors).toHaveLength(2)
    expect(errors[0]).toContain('VESPERA_VERSION = "0.4"')
    expect(errors[0]).toContain('expected 0.5 or 0.6')
  })

  it('rejects a minor ahead of the release without a pending bump', () => {
    const errors = checkDocumentedVersions(docs('0.7'), '0.6.0', 'none')
    expect(errors).toHaveLength(2)
    expect(errors[0]).toContain('expected 0.6;')
  })

  it('rejects a mismatched bridge version only', () => {
    expect(
      checkDocumentedVersions(docs('0.6', '0.5.9'), '0.6.0', 'none'),
    ).toEqual([expect.stringContaining('VESPERA_BRIDGE_VERSION = "0.5.9"')])
  })

  it('rejects values that are not versions', () => {
    expect(
      checkDocumentedVersions({ VESPERA_VERSION: 'latest' }, '0.6.0', 'none'),
    ).toEqual(['landing VESPERA_VERSION = "latest" is not a version'])
  })
})
