import { describe, expect, it } from 'bun:test'

import {
  evaluate,
  isRegression,
  median,
  pairedChanges,
  parseRound,
  renderMarkdown,
} from './jni-bench-compare'

const T = { mean: 0.3, lower: 0.2 }

describe('median', () => {
  it.each([
    [[3], 3],
    [[5, 1, 3], 3],
    [[4, 1, 3, 2], 2.5],
  ])('%p -> %p', (values, expected) => {
    expect(median(values)).toBe(expected)
  })
})

describe('pairedChanges', () => {
  it('pairs round i with round i and skips incomplete rounds', () => {
    expect(
      pairedChanges([100, 200, undefined, 100], [110, 100, 50, 150]),
    ).toEqual([0.10000000000000009, -0.5, 0.5])
  })
})

describe('isRegression', () => {
  it.each<[string, number[], boolean]>([
    ['every round well above both limits', [0.35, 0.4, 0.45, 0.32, 0.5], true],
    [
      'median above +30% but one round below +20%',
      [0.35, 0.4, 0.45, 0.1, 0.5],
      false,
    ],
    [
      'every round above +20% but median below +30%',
      [0.21, 0.25, 0.28, 0.22, 0.4],
      false,
    ],
    [
      'median exactly at the limit does not fail',
      [0.3, 0.3, 0.3, 0.3, 0.3],
      false,
    ],
    ['improvement', [-0.2, -0.1, -0.3, -0.15, -0.05], false],
    ['no data', [], false],
  ])('%s', (_name, changes, expected) => {
    expect(isRegression(changes, T)).toBe(expected)
  })
})

describe('parseRound', () => {
  it('accepts raw RESULT lines and plain pairs, ignores noise', () => {
    const r = parseRound(
      'RESULT sync_get 2900\nnoise line here\ndirect_get 2100\r\n\n',
    )
    expect([...r.entries()]).toEqual([
      ['sync_get', 2900],
      ['direct_get', 2100],
    ])
  })
})

describe('evaluate', () => {
  const round = (sync: number, direct: number) =>
    new Map([
      ['sync_get', sync],
      ['direct_get', direct],
    ])

  it('flags only the mode that regressed in every round', () => {
    const base = [round(1000, 1000), round(1000, 1000), round(1000, 1000)]
    const head = [round(1500, 1050), round(1400, 900), round(1600, 1000)]
    const verdicts = evaluate(base, head, T)
    const byMode = Object.fromEntries(
      verdicts.map((v) => [v.mode, v.regressed]),
    )
    expect(byMode).toEqual({ sync_get: true, direct_get: false })
    expect(renderMarkdown(verdicts, T)).toContain(
      '| `sync_get` | 1000 | 1500 | +50.00% | +40.00% |',
    )
  })

  it('ignores a mode present on one side only', () => {
    const verdicts = evaluate([new Map([['a', 1]])], [new Map([['b', 2]])], T)
    expect(verdicts).toEqual([])
  })
})
