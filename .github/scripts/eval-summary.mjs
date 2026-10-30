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
const percent = (num, den) => (den ? `${Math.round((100 * num) / den)}%` : 'n/a')

const summary = [
  '| Eval | Provider | Passed | Failed | Errors | Metrics |',
  '| --- | --- | --- | --- | --- | --- |'
]
const details = []

for (const file of process.argv.slice(2)) {
  const name = basename(file, '.json')
  const { results } = JSON.parse(readFileSync(file, 'utf8'))

  for (const prompt of results.prompts) {
    const m = prompt.metrics
    const total = m.testPassCount + m.testFailCount + m.testErrorCount
    const metrics = Object.entries(m.namedScores)
      .map(([metric, score]) => `${metric} ${percent(score, m.namedScoresCount[metric])}`)
      .join(' · ')
    summary.push(
      `| ${name} | ${cell(prompt.provider)} | ${m.testPassCount}/${total} (${percent(m.testPassCount, total)}) | ${m.testFailCount} | ${m.testErrorCount} | ${metrics} |`
    )
  }

  const failing = results.results.filter(result => !result.success)
  if (failing.length === 0) continue
  const rows = failing.slice(0, MAX_CASES).map(result => {
    const failed = (result.gradingResult?.componentResults ?? [])
      .filter(component => !component.pass)
      .map(component => component.assertion?.metric)
      .filter(Boolean)
    // promptfoo's ResultFailureReason: 1 = assertion failed, 2 = provider error.
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
