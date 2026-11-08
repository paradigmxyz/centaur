import { describe, expect, test } from 'bun:test'
import type { ChatSDKStreamChunk } from '@centaur/rendering'
import {
  buildGenerativeUiCard,
  DEFAULT_GENERATIVE_UI_LIMITS,
  extractGenerativeUiBlocks,
  generativeUiBlockAsCode,
  generativeUiCatalog,
  generativeUiCatalogPrompt,
  GenerativeUiSpecError,
  parseGenerativeUiLimits,
  parseGenerativeUiSpec,
  stripGenerativeUiBlocks
} from '../src/generative-ui'

const TABLE_SPEC = {
  root: 'card',
  elements: {
    card: { type: 'Card', props: { title: 'October close' }, children: ['kpis', 'venues'] },
    kpis: { type: 'Fields', props: {}, children: ['total'] },
    total: { type: 'Field', props: { label: 'Total paid', value: 31150 }, children: [] },
    venues: {
      type: 'Table',
      props: { headers: ['Venue', 'USD'], rows: [['Ethena', 12400], ['Maple', 9850]] },
      children: []
    }
  }
}

const TABLE_BLOCK = JSON.stringify(TABLE_SPEC, null, 2)

function fenced(body: string, language = 'json-render'): string {
  return `\`\`\`${language}\n${body}\n\`\`\``
}

describe('extractGenerativeUiBlocks', () => {
  test('leaves an answer without a block untouched', () => {
    const markdown = 'Plain answer with a ```json\n{"a":1}\n``` code block.'
    expect(extractGenerativeUiBlocks(markdown)).toEqual({ markdown, blockCount: 0 })
  })

  test('removes a single block and joins the prose around it', () => {
    const markdown = `Summary first.\n\n${fenced(TABLE_BLOCK)}\n\nNotes after.`
    const result = extractGenerativeUiBlocks(markdown)
    expect(result.blockCount).toBe(1)
    expect(result.block).toBe(TABLE_BLOCK)
    expect(result.markdown).toBe('Summary first.\n\nNotes after.')
  })

  test('returns empty markdown when the answer is only a block', () => {
    const result = extractGenerativeUiBlocks(`${fenced(TABLE_BLOCK)}\n`)
    expect(result).toEqual({ markdown: '', block: TABLE_BLOCK, blockCount: 1 })
  })

  test('keeps every block in place when there are two', () => {
    const markdown = `${fenced('{"root":"a","elements":{}}')}\nand\n${fenced('{"root":"b","elements":{}}')}`
    expect(extractGenerativeUiBlocks(markdown)).toEqual({ markdown, blockCount: 2 })
  })

  test('ignores an unterminated fence', () => {
    const markdown = 'Text\n```json-render\n{"root":"a"'
    expect(extractGenerativeUiBlocks(markdown)).toEqual({ markdown, blockCount: 0 })
  })
})

describe('parseGenerativeUiSpec', () => {
  test('accepts a flat spec object', () => {
    expect(parseGenerativeUiSpec(TABLE_BLOCK)).toEqual(TABLE_SPEC)
  })

  test('compiles the JSONL patch stream the catalog prompt describes', () => {
    const patches = [
      { op: 'add', path: '/root', value: 'card' },
      { op: 'add', path: '/elements/card', value: { type: 'Card', props: { title: 'T' }, children: ['t'] } },
      { op: 'add', path: '/elements/t', value: { type: 'Text', props: { content: 'hi' }, children: [] } }
    ]
    const spec = parseGenerativeUiSpec(patches.map(patch => JSON.stringify(patch)).join('\n'))
    expect(spec.root).toBe('card')
    expect(Object.keys(spec.elements)).toEqual(['card', 't'])
  })

  test('rejects malformed JSON and a spec without root or elements', () => {
    expect(() => parseGenerativeUiSpec('{"root": "card", "elements": ')).toThrow(GenerativeUiSpecError)
    try {
      parseGenerativeUiSpec('{"root": "card", "elements": ')
    } catch (error) {
      expect((error as GenerativeUiSpecError).reason).toBe('malformed_json')
    }
    try {
      parseGenerativeUiSpec('{"title": "no spec here"}')
    } catch (error) {
      expect((error as GenerativeUiSpecError).reason).toBe('invalid_spec')
      expect((error as GenerativeUiSpecError).issues).toHaveLength(2)
    }
    expect(() => parseGenerativeUiSpec('{"op":"add","path":"/root","value":"a"}\nnot json')).toThrow(
      /line 2/
    )
  })
})

describe('buildGenerativeUiCard', () => {
  test('converts a valid spec into a Chat SDK card', () => {
    const card = buildGenerativeUiCard(TABLE_BLOCK)
    expect(card.type).toBe('card')
    expect(card.title).toBe('October close')
    expect(card.children.map(child => child.type)).toEqual(['fields', 'table'])
  })

  test('rejects specs past the configured limits with the json-render issues', () => {
    const spec = {
      root: 'card',
      elements: {
        card: { type: 'Card', props: {}, children: ['t1', 't2'] },
        t1: { type: 'Table', props: { headers: ['a'], rows: [['1']] }, children: [] },
        t2: { type: 'Table', props: { headers: ['a'], rows: [['1']] }, children: [] }
      }
    }
    expect(() => buildGenerativeUiCard(JSON.stringify(spec))).toThrow(GenerativeUiSpecError)
    try {
      buildGenerativeUiCard(JSON.stringify(spec), DEFAULT_GENERATIVE_UI_LIMITS)
    } catch (error) {
      expect((error as GenerativeUiSpecError).reason).toBe('invalid_spec')
      expect((error as GenerativeUiSpecError).issues).toEqual(['at most 1 Table per message'])
    }
    expect(buildGenerativeUiCard(JSON.stringify(spec), { tables: 2 }).children).toHaveLength(2)
  })

  test('rejects interactive buttons, which slackbotv2 cannot route', () => {
    const spec = {
      root: 'card',
      elements: {
        card: { type: 'Card', props: {}, children: ['actions'] },
        actions: { type: 'Actions', props: {}, children: ['b'] },
        b: { type: 'Button', props: { id: 'go', label: 'Go' }, children: [] }
      }
    }
    expect(() => buildGenerativeUiCard(JSON.stringify(spec))).toThrow(/unknown component "Button"/)
    expect(generativeUiCatalog.componentNames).not.toContain('Button')
    expect(generativeUiCatalog.componentNames).toContain('LinkButton')
  })
})

describe('generativeUiCatalogPrompt', () => {
  test('describes every catalog component and the fenced-block contract', () => {
    const prompt = generativeUiCatalogPrompt()
    for (const name of generativeUiCatalog.componentNames) {
      expect(prompt).toContain(`- ${name}:`)
    }
    expect(prompt).not.toContain('- Button:')
    expect(prompt).toContain('json-render')
    expect(prompt).toContain('tables: 1, charts: 2')
    expect(prompt).toContain('plain text only')
  })
})

describe('parseGenerativeUiLimits', () => {
  test('defaults when unset and validates the JSON object', () => {
    expect(parseGenerativeUiLimits(undefined)).toEqual({ tables: 1, charts: 2 })
    expect(parseGenerativeUiLimits(' ')).toEqual({ tables: 1, charts: 2 })
    expect(parseGenerativeUiLimits('{"tables":2,"chartPoints":12}')).toEqual({ tables: 2, chartPoints: 12 })
    expect(() => parseGenerativeUiLimits('[1]')).toThrow('SLACKBOTV2_GENERATIVE_UI_LIMITS')
    expect(() => parseGenerativeUiLimits('{"tables":"1"}')).toThrow('positive integer')
    expect(() => parseGenerativeUiLimits('{"buttons":1}')).toThrow('unknown limit')
  })
})

describe('generativeUiBlockAsCode', () => {
  test('wraps the block as a json code block', () => {
    expect(generativeUiBlockAsCode('{"a":1}\n')).toBe('```json\n{"a":1}\n```')
  })
})

async function* chunks(parts: string[]): AsyncIterable<ChatSDKStreamChunk> {
  for (const part of parts) yield { type: 'markdown_text', text: part }
}

async function collect(
  parts: string[],
  capture = { blocks: [] as string[] }
): Promise<{ blocks: string[]; text: string }> {
  let text = ''
  for await (const chunk of stripGenerativeUiBlocks(chunks(parts), capture)) {
    if (chunk.type === 'markdown_text') text += chunk.text
  }
  return { blocks: capture.blocks, text }
}

describe('stripGenerativeUiBlocks', () => {
  test('passes a stream without a block through unchanged', async () => {
    const parts = ['Hello ', 'world\n', '```json\n{"a":1}\n```\n', 'tail without newline']
    expect(await collect(parts)).toEqual({ blocks: [], text: parts.join('') })
  })

  test('removes a block split across deltas and captures its body', async () => {
    const parts = [
      'Before the card.\n\n``',
      '`json-ren',
      'der\n{"root":"card",',
      '\n"elements":{}}\n``',
      '`\n\nAfter the card.'
    ]
    expect(await collect(parts)).toEqual({
      blocks: ['{"root":"card",\n"elements":{}}'],
      text: 'Before the card.\n\nAfter the card.'
    })
  })

  test('does not hold back ordinary text that merely starts with backticks', async () => {
    const seen: string[] = []
    const capture = { blocks: [] as string[] }
    const source = (async function* () {
      yield { type: 'markdown_text', text: 'line one\n' } as ChatSDKStreamChunk
      yield { type: 'markdown_text', text: '```sh\nls\n```\nmore' } as ChatSDKStreamChunk
    })()
    for await (const chunk of stripGenerativeUiBlocks(source, capture)) {
      if (chunk.type === 'markdown_text') seen.push(chunk.text)
    }
    expect(seen.join('')).toBe('line one\n```sh\nls\n```\nmore')
    expect(capture.blocks).toEqual([])
  })

  test('flushes an unterminated block as text when the stream ends', async () => {
    const parts = ['Intro\n```json-render\n{"root":"card"']
    expect(await collect(parts)).toEqual({ blocks: [], text: 'Intro\n```json-render\n{"root":"card"' })
  })

  test('strips only the first block and records later ones verbatim', async () => {
    const first = '{"root":"a","elements":{}}'
    const second = '{"root":"b","elements":{}}'
    const parts = [`${fenced(first)}\nbetween\n${fenced(second)}\n`]
    expect(await collect(parts)).toEqual({
      blocks: [first, second],
      text: `between\n${fenced(second)}\n`
    })
  })

  test('keeps task and plan chunks in order around the block', async () => {
    const capture = { blocks: [] as string[] }
    const source = (async function* () {
      yield { type: 'plan_update', title: 'Plan' } as ChatSDKStreamChunk
      yield { type: 'markdown_text', text: `Text\n${fenced('{"root":"a","elements":{}}')}` } as ChatSDKStreamChunk
      yield { type: 'task_update', id: 't', title: 'Task', status: 'complete' } as ChatSDKStreamChunk
    })()
    const out: ChatSDKStreamChunk[] = []
    for await (const chunk of stripGenerativeUiBlocks(source, capture)) out.push(chunk)
    expect(out.map(chunk => chunk.type)).toEqual(['plan_update', 'markdown_text', 'task_update'])
    expect(out[1]).toEqual({ type: 'markdown_text', text: 'Text\n' })
    expect(capture.blocks).toEqual(['{"root":"a","elements":{}}'])
  })
})

describe('generativeUiTurnNote', () => {
  test('carries the fenced-block contract, the catalog components and the structural rules', async () => {
    const { generativeUiTurnNote, generativeUiComponentList } = await import('../src/generative-ui')
    const note = generativeUiTurnNote()
    for (const name of generativeUiCatalog.componentNames) {
      expect(note).toContain(`- ${name}:`)
    }
    expect(note).not.toContain('- Button:')
    expect(note).toContain('tagged json-render')
    expect(note).toContain('"root"')
    expect(note).toContain('tables: 1, charts: 2')
    expect(note).toContain('plain text only')
    expect(note).toContain('Field elements MUST be children of a Fields element')
    expect(note).not.toContain('$bindState')
    expect(generativeUiComponentList()).toStartWith('AVAILABLE COMPONENTS (11):')
    expect(generativeUiTurnNote({ tables: 2 })).toContain('tables: 2.')
    // It rides on every turn, so it has to stay compact.
    expect(note.length).toBeLessThan(4_000)
  })
})

describe('GENERATIVE_UI.md', () => {
  test('matches the generator output', async () => {
    const { renderGenerativeUiDoc } = await import('../src/generative-ui-doc')
    const current = await Bun.file(new URL('../GENERATIVE_UI.md', import.meta.url)).text()
    expect(current).toBe(renderGenerativeUiDoc())
  })
})
