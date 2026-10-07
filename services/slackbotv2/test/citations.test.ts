import { describe, expect, it } from 'bun:test'
import type { ChatSDKStreamChunk } from '@centaur/rendering'
import { stripSlackCitations, stripSlackCitationsFromStream } from '../src/citations'

const MARKERS = [
  'citeturn0search0',
  'citeturn0search0turn1search2',
  'cite:ship:turn0search0:walking:'
]

async function* chunks(source: ChatSDKStreamChunk[]): AsyncIterable<ChatSDKStreamChunk> {
  yield* source
}

async function collect(source: ChatSDKStreamChunk[]): Promise<ChatSDKStreamChunk[]> {
  return Array.fromAsync(stripSlackCitationsFromStream(chunks(source)))
}

describe('Slack citations', () => {
  it('removes internal references without inventing links or changing real links', () => {
    for (const marker of MARKERS) {
      expect(stripSlackCitations(`Answer.${marker} [Source](https://example.com/source)`))
        .toBe('Answer. [Source](https://example.com/source)')
    }
  })

  it('removes consecutive citations and preserves unrelated text and emoji', () => {
    expect(stripSlackCitations(`Answer.${MARKERS.join('')} Next :ship: :walking:.`))
      .toBe('Answer. Next :ship: :walking:.')
    expect(stripSlackCitations('Other chartdata and cite example.'))
      .toBe('Other chartdata and cite example.')
  })

  it('never emits citation fragments at any streaming split boundary', async () => {
    for (const marker of MARKERS) {
      for (let boundary = 1; boundary < marker.length; boundary++) {
        expect(await collect([
          { type: 'markdown_text', text: `Answer.${marker.slice(0, boundary)}` },
          { type: 'markdown_text', text: `${marker.slice(boundary)} Next.` }
        ])).toEqual([
          { type: 'markdown_text', text: 'Answer.' },
          { type: 'markdown_text', text: ' Next.' }
        ])
      }
    }
  })

  it('handles character-sized deltas and interleaved task updates', async () => {
    const task: ChatSDKStreamChunk = { type: 'task_update', id: 'task', title: 'Search', status: 'complete' }
    const source: ChatSDKStreamChunk[] = [
      { type: 'markdown_text', text: 'Answer.' },
      ...Array.from(MARKERS[0]!, text => ({ type: 'markdown_text' as const, text })),
      task,
      { type: 'markdown_text', text: ' Next.' }
    ]
    expect(await collect(source)).toEqual([
      { type: 'markdown_text', text: 'Answer.' },
      task,
      { type: 'markdown_text', text: ' Next.' }
    ])
  })

  it('drops unfinished citations but flushes an unrelated partial prefix', async () => {
    for (const marker of ['citeturn0search0', 'cite:ship:turn0search0']) {
      expect(stripSlackCitations(`Answer.${marker}`)).toBe('Answer.')
      expect(await collect([{ type: 'markdown_text', text: `Answer.${marker}` }]))
        .toEqual([{ type: 'markdown_text', text: 'Answer.' }])
    }
    expect(await collect([{ type: 'markdown_text', text: 'Literal cit' }])).toEqual([
      { type: 'markdown_text', text: 'Literal ' },
      { type: 'markdown_text', text: 'cit' }
    ])
  })

  it('preserves prose following a malformed or interrupted citation', async () => {
    for (const marker of ['citeturn0search0', 'cite:ship:turn0search0']) {
      expect(stripSlackCitations(`Answer.${marker}\nNext paragraph.`))
        .toBe('Answer.\nNext paragraph.')
      expect(await collect([
        { type: 'markdown_text', text: `Answer.${marker}` },
        { type: 'markdown_text', text: '\nNext paragraph.' }
      ])).toEqual([
        { type: 'markdown_text', text: 'Answer.' },
        { type: 'markdown_text', text: '\nNext paragraph.' }
      ])
    }
  })
})
