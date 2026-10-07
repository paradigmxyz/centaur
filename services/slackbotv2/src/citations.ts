import type { ChatSDKStreamChunk } from '@centaur/rendering'

const CITATION_PREFIXES = ['cite', 'cite:ship:']
const CITATION_ENDINGS = ['', ':walking:']

export function stripSlackCitations(text: string): string {
  const filter = new SlackCitationFilter()
  return filter.push(text) + filter.finish()
}

export async function* stripSlackCitationsFromStream(
  stream: AsyncIterable<ChatSDKStreamChunk>
): AsyncIterable<ChatSDKStreamChunk> {
  const filter = new SlackCitationFilter()
  for await (const chunk of stream) {
    if (chunk.type !== 'markdown_text') {
      yield chunk
      continue
    }
    const text = filter.push(chunk.text)
    if (text) yield { ...chunk, text }
  }
  const text = filter.finish()
  if (text) yield { type: 'markdown_text', text }
}

class SlackCitationFilter {
  private pending = ''

  push(text: string): string {
    this.pending += text
    let visible = ''
    while (this.pending) {
      const start = this.pending.indexOf('')
      if (start < 0) {
        visible += this.pending
        this.pending = ''
        break
      }
      visible += this.pending.slice(0, start)
      this.pending = this.pending.slice(start)
      const prefixIndex = CITATION_PREFIXES.findIndex(prefix => this.pending.startsWith(prefix))
      if (prefixIndex < 0) {
        if (CITATION_PREFIXES.some(prefix => prefix.startsWith(this.pending))) break
        visible += this.pending[0]
        this.pending = this.pending.slice(1)
        continue
      }
      const ending = CITATION_ENDINGS[prefixIndex]!
      const prefixLength = CITATION_PREFIXES[prefixIndex]!.length
      const end = this.pending.indexOf(ending, prefixLength)
      const invalid = this.pending.slice(prefixLength).search(/[^A-Za-z0-9_:.\-]/u)
      if (invalid >= 0 && (end < 0 || prefixLength + invalid < end)) {
        this.pending = this.pending.slice(prefixLength + invalid)
        continue
      }
      if (end < 0) break
      this.pending = this.pending.slice(end + ending.length)
    }
    return visible
  }

  finish(): string {
    const text = CITATION_PREFIXES.some(prefix => this.pending.startsWith(prefix))
      ? ''
      : this.pending
    this.pending = ''
    return text
  }
}
