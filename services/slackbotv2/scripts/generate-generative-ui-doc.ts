/**
 * Writes GENERATIVE_UI.md: the agent-facing contract for json-render cards and
 * the catalog prompt generated from the catalog slackbotv2 validates against.
 *
 *   bun scripts/generate-generative-ui-doc.ts          # write the file
 *   bun scripts/generate-generative-ui-doc.ts --check  # fail if it is stale
 */
import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { renderGenerativeUiDoc } from '../src/generative-ui-doc'

const path = join(import.meta.dir, '..', 'GENERATIVE_UI.md')
const expected = renderGenerativeUiDoc()
if (process.argv.includes('--check')) {
  const current = readFileSync(path, 'utf8')
  if (current !== expected) {
    console.error('GENERATIVE_UI.md is stale; run bun scripts/generate-generative-ui-doc.ts')
    process.exit(1)
  }
  console.log('GENERATIVE_UI.md is current')
} else {
  writeFileSync(path, expected)
  console.log(`wrote ${path}`)
}
