/**
 * Paired comparison + pass/fail rule for the JNI dispatch regression gate.
 *
 *   bun scripts/jni-bench-compare.ts <results-dir>
 *
 * `<results-dir>` holds one file per (side, round) written by
 * `scripts/jni-bench-interleaved.sh`: `base-<round>.txt` / `head-<round>.txt`,
 * each line `<mode> <ns_per_op>` (the harness's `RESULT` lines). Round `i` of
 * the base is paired with round `i` of the head — the two JVMs ran seconds
 * apart, so the paired change `c_i = head_i / base_i - 1` cancels runner drift.
 *
 * Decision per mode (the same rule as the Rust criterion gate,
 * `scripts/bench-interleaved.sh`):
 *   FAIL  iff  median(c_i) > REGRESS_MEAN  (default +30%)
 *         AND  min(c_i)    > REGRESS_LOWER (default +20%)
 * The min of N paired samples is the distribution-free lower confidence limit
 * of the median (sign test, 93.75% for N = 5): a regression must show up in
 * EVERY round, so one noisy round cannot trip the gate.
 *
 * Prints a markdown report (also appended to `$GITHUB_STEP_SUMMARY` when set)
 * and exits 1 when any mode regresses, 2 on unusable input.
 */
import { appendFileSync, readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

export interface Thresholds {
  mean: number
  lower: number
}

export interface ModeVerdict {
  mode: string
  changes: number[]
  median: number
  min: number
  baseMedianNs: number
  headMedianNs: number
  regressed: boolean
}

export function median(values: readonly number[]): number {
  if (values.length === 0) throw new Error('median of an empty list')
  const s = [...values].sort((a, b) => a - b)
  const mid = s.length >> 1
  return s.length % 2 ? s[mid] : (s[mid - 1] + s[mid]) / 2
}

/** Paired per-round changes `head_i / base_i - 1` (rounds missing a side are skipped). */
export function pairedChanges(
  base: readonly (number | undefined)[],
  head: readonly (number | undefined)[],
): number[] {
  const out: number[] = []
  for (let i = 0; i < Math.min(base.length, head.length); i++) {
    const b = base[i]
    const h = head[i]
    if (b === undefined || h === undefined || !(b > 0)) continue
    out.push(h / b - 1)
  }
  return out
}

export function isRegression(
  changes: readonly number[],
  t: Thresholds,
): boolean {
  if (changes.length === 0) return false
  return median(changes) > t.mean && Math.min(...changes) > t.lower
}

/** Parses `<mode> <ns>` lines into a map (blank / malformed lines ignored). */
export function parseRound(text: string): Map<string, number> {
  const out = new Map<string, number>()
  for (const line of text.split(/\r?\n/)) {
    const m = /^\s*(?:RESULT\s+)?(\S+)\s+(\d+(?:\.\d+)?)\s*$/.exec(line)
    if (m) out.set(m[1], Number(m[2]))
  }
  return out
}

export function evaluate(
  base: readonly Map<string, number>[],
  head: readonly Map<string, number>[],
  t: Thresholds,
): ModeVerdict[] {
  const modes = new Set<string>()
  for (const r of head) for (const k of r.keys()) modes.add(k)
  const verdicts: ModeVerdict[] = []
  for (const mode of modes) {
    const b = base.map((r) => r.get(mode))
    const h = head.map((r) => r.get(mode))
    const changes = pairedChanges(b, h)
    if (changes.length === 0) continue
    const bs = b.filter((v): v is number => v !== undefined)
    const hs = h.filter((v): v is number => v !== undefined)
    verdicts.push({
      mode,
      changes,
      median: median(changes),
      min: Math.min(...changes),
      baseMedianNs: median(bs),
      headMedianNs: median(hs),
      regressed: isRegression(changes, t),
    })
  }
  return verdicts
}

const pct = (v: number) => `${v >= 0 ? '+' : ''}${(v * 100).toFixed(2)}%`

export function renderMarkdown(
  verdicts: readonly ModeVerdict[],
  t: Thresholds,
): string {
  const lines = [
    '## JNI dispatch regression gate',
    '',
    `Paired interleaved rounds (merge base vs PR head, one fresh JVM per side per round). ` +
      `A mode fails when the median paired change > ${pct(t.mean)} **and** every round > ${pct(t.lower)}.`,
    '',
    '| mode | base ns/op | head ns/op | median change | min change | rounds | verdict |',
    '|---|---:|---:|---:|---:|---|---|',
  ]
  for (const v of verdicts) {
    lines.push(
      `| \`${v.mode}\` | ${Math.round(v.baseMedianNs)} | ${Math.round(v.headMedianNs)} | ` +
        `${pct(v.median)} | ${pct(v.min)} | ${v.changes.map(pct).join(' ')} | ` +
        `${v.regressed ? ':x: regression' : ':white_check_mark: ok'} |`,
    )
  }
  if (verdicts.length === 0) lines.push('', '_No comparable modes found._')
  return `${lines.join('\n')}\n`
}

function readSide(dir: string, side: 'base' | 'head'): Map<string, number>[] {
  const rounds: Map<string, number>[] = []
  for (const name of readdirSync(dir)) {
    const m = new RegExp(`^${side}-(\\d+)\\.txt$`).exec(name)
    if (m)
      rounds[Number(m[1]) - 1] = parseRound(
        readFileSync(join(dir, name), 'utf8'),
      )
  }
  return Array.from(rounds, (r) => r ?? new Map())
}

function main(): number {
  const dir = process.argv[2]
  if (!dir) {
    console.error('usage: bun scripts/jni-bench-compare.ts <results-dir>')
    return 2
  }
  const t: Thresholds = {
    mean: Number(process.env.REGRESS_MEAN ?? '0.30'),
    lower: Number(process.env.REGRESS_LOWER ?? '0.20'),
  }
  const verdicts = evaluate(readSide(dir, 'base'), readSide(dir, 'head'), t)
  const report = renderMarkdown(verdicts, t)
  process.stdout.write(report)
  const summary = process.env.GITHUB_STEP_SUMMARY
  if (summary) appendFileSync(summary, report)
  if (verdicts.length === 0) {
    console.error('::error::No comparable JNI bench results found.')
    return 2
  }
  for (const v of verdicts.filter((x) => x.regressed)) {
    console.error(
      `::error::JNI performance regression: ${v.mode} median ${pct(v.median)} with every paired round > ${pct(t.lower)}`,
    )
  }
  return verdicts.some((v) => v.regressed) ? 1 : 0
}

if (import.meta.main) process.exit(main())
