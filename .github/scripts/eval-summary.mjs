#!/usr/bin/env node
// Renders promptfoo JSON outputs (`promptfoo eval -o <name>.json`) as a
// Markdown PR comment. Usage: eval-summary.mjs <results.json>...
import { readFileSync } from 'node:fs'
import { basename } from 'node:path'

const MAX_CASES = 30
const MAX_CELL = 200

const cell = value => {
  const text = String(value ?? '').replace(/\s+/g, ' ').replace(/\|/g, '\\|')
  return text.length > MAX_CELL ? `${text.slice(0, MAX_CELL)}…` : text
}
const isHoldout = result => String(result.testCase?.description ?? '').startsWith('holdout/')
const percent = (num, den) => (den ? `${Math.round((100 * num) / den)}%` : 'n/a')

const summary = [
  '| Eval | Provider | Passed | Failed | Errors | Metrics |',
  '| --- | --- | --- | --- | --- | --- |'
]
const details = []

for (const file of process.argv.slice(2)) {
  const name = basename(file, '.json')
  const { results } = JSON.parse(readFileSync(file, 'utf8'))

  // Cases described `holdout/...` are excluded from prompt tuning, so they are
  // reported separately from the tuning cases.
  const hasHoldout = results.results.some(result => isHoldout(result))
  const groups = new Map()
  for (const result of results.results) {
    const provider = result.provider?.label ?? result.provider?.id
    const split = hasHoldout ? (isHoldout(result) ? ' (holdout)' : ' (tuning)') : ''
    const key = `${split === ' (holdout)' ? 1 : 0}\0${provider}`
    if (!groups.has(key)) {
      groups.set(key, { eval: `${name}${split}`, provider, pass: 0, fail: 0, error: 0, scores: {} })
    }
    const group = groups.get(key)
    // promptfoo's ResultFailureReason: 1 = assertion failed, 2 = provider error.
    if (result.success) group.pass++
    else if (result.failureReason === 2) group.error++
    else group.fail++
    for (const [metric, score] of Object.entries(result.namedScores ?? {})) {
      const sum = (group.scores[metric] ??= { score: 0, count: 0 })
      sum.score += score
      sum.count++
    }
  }
  for (const g of [...groups.keys()].sort().map(key => groups.get(key))) {
    const total = g.pass + g.fail + g.error
    const metrics = Object.entries(g.scores)
      .map(([metric, { score, count }]) => `${metric} ${percent(score, count)}`)
      .join(' · ')
    summary.push(
      `| ${g.eval} | ${cell(g.provider)} | ${g.pass}/${total} (${percent(g.pass, total)}) | ${g.fail} | ${g.error} | ${metrics} |`
    )
  }

  const failing = results.results.filter(result => !result.success)
  if (failing.length === 0) continue
  const rows = failing.slice(0, MAX_CASES).map(result => {
    const failed = (result.gradingResult?.componentResults ?? [])
      .filter(component => !component.pass)
      .map(component => component.assertion?.metric)
      .filter(Boolean)
    const reason = result.failureReason === 2
      ? `error: ${result.error}`
      : `${failed.join(', ') || 'failed'}: got ${JSON.stringify(result.response?.output)}`
    return `| ${cell(result.testCase.description)} | ${cell(result.provider?.label ?? result.provider?.id)} | ${cell(reason)} |`
  })
  if (failing.length > MAX_CASES) rows.push(`| … ${failing.length - MAX_CASES} more | | |`)
  details.push(
    '',
    `<details><summary>${name}: ${failing.length} failing or errored cases</summary>`,
    '',
    '| Case | Provider | Reason |',
    '| --- | --- | --- |',
    ...rows,
    '',
    '</details>'
  )
}

console.log([...summary, ...details].join('\n'))
