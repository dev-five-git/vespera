// Every published crate ships a copy of the root LICENSE: cargo packages only
// the crate directory and Apache-2.0 §4(a) requires the license text in each
// distribution. Copies, not symlinks (Windows checkouts), so keep them equal.
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { describe, expect, it } from 'bun:test'

const root = join(import.meta.dir, '..')
const read = (path: string) =>
  readFileSync(join(root, path), 'utf8').replace(/\r\n/g, '\n')

describe('crate LICENSE copies', () => {
  it.each([
    'vespera',
    'vespera_core',
    'vespera_macro',
    'vespera_inprocess',
    'vespera_jni',
  ])('crates/%s/LICENSE matches the root LICENSE', (crate) => {
    expect(read(`crates/${crate}/LICENSE`)).toBe(read('LICENSE'))
  })
})
