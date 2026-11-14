import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

import { describe, expect, it } from 'bun:test'

import {
  DOC_VERSION_PATTERNS,
  VESPERA_BRIDGE_PLUGIN_VERSION,
  VESPERA_BRIDGE_VERSION,
  VESPERA_VERSION,
} from '@/constants'

const DOCS_DIR = join(
  import.meta.dir,
  '..',
  'app',
  'documentation',
  '[...name]',
)

const docs = readdirSync(DOCS_DIR)
  .filter((file) => file.endsWith('.mdx'))
  .map((file) => ({
    file,
    content: readFileSync(join(DOCS_DIR, file), 'utf-8'),
  }))

const CONSTANTS = {
  VESPERA_VERSION,
  VESPERA_BRIDGE_VERSION,
  VESPERA_BRIDGE_PLUGIN_VERSION,
}

describe('documentation versions', () => {
  it('should find documentation pages', () => {
    expect(docs.length).toBeGreaterThan(0)
  })

  it('should quote the current versions in every code sample', () => {
    const found: { file: string; version: string; expected: string }[] = []
    for (const { file, content } of docs) {
      for (const [pattern, constant] of DOC_VERSION_PATTERNS) {
        for (const match of content.matchAll(pattern)) {
          found.push({ file, version: match[1], expected: CONSTANTS[constant] })
        }
      }
    }
    expect(found.length).toBeGreaterThan(0)
    for (const { file, version, expected } of found) {
      expect(`${file}: ${version}`).toBe(`${file}: ${expected}`)
    }
  })
})
