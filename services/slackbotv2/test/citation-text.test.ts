import { describe, expect, it } from 'bun:test'
import { CitationTextFilter, stripUnresolvedCitations } from '../src/citation-text'

describe('citation text', () => {
  const markers = [
    '\uE200cite\uE202turn0search8\uE201',
    '\uE200cite\uE202turn0search8\uE202turn0search7\uE201',
    '\uE200cite:ship:turn0search8:walking:'
  ]

  for (const marker of markers) {
    it(`removes an unresolved citation ${JSON.stringify(marker)}`, () => {
      expect(stripUnresolvedCitations(`Before.${marker} After.`)).toBe('Before. After.')
    })

    it(`buffers every split of ${JSON.stringify(marker)}`, () => {
      for (let split = 1; split < marker.length; split += 1) {
        const filter = new CitationTextFilter()
        expect(filter.append(`Before.${marker.slice(0, split)}`)).toBe('Before.')
        expect(filter.append(`${marker.slice(split)} After.`)).toBe(' After.')
        expect(filter.finish()).toBe('')
      }
    })

    it(`removes ${JSON.stringify(marker)} streamed one character at a time`, () => {
      const filter = new CitationTextFilter()
      const output = [...`Before.${marker}${marker} After.`]
        .map(character => filter.append(character))
        .join('') + filter.finish()
      expect(output).toBe('Before. After.')
    })
  }

  it('preserves links, code, emoji shortcodes, and unrelated private-use characters', () => {
    const text = '[Docs](https://example.com) `code` 🚢 🚶 :ship: :walking: \uE200other\uE201'
    expect(stripUnresolvedCitations(text)).toBe(text)
  })

  it('discards truncated citations at completion', () => {
    for (const marker of markers) {
      for (let length = 1; length < marker.length; length += 1) {
        expect(stripUnresolvedCitations(`Before.${marker.slice(0, length)}`)).toBe('Before.')
      }
    }
  })

  it('releases unrelated text after a partial citation prefix', () => {
    const filter = new CitationTextFilter()
    expect(filter.append('Before.\uE200ci')).toBe('Before.')
    expect(filter.append('der After.')).toBe('\uE200cider After.')
    expect(filter.finish()).toBe('')
  })
})
