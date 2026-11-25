const CITATION_PREFIXES = ['\uE200cite\uE202', '\uE200cite:ship:']

export class CitationTextFilter {
  private pending = ''

  append(text: string): string {
    return this.consume(this.pending + text, false)
  }

  finish(): string {
    return this.consume(this.pending, true)
  }

  private consume(text: string, final: boolean): string {
    this.pending = ''
    let output = ''
    let offset = 0
    while (offset < text.length) {
      const start = text.indexOf('\uE200', offset)
      if (start < 0) return output + text.slice(offset)
      output += text.slice(offset, start)
      const remaining = text.slice(start)
      const prefix = CITATION_PREFIXES.find(prefix => remaining.startsWith(prefix))
      if (!prefix) {
        if (CITATION_PREFIXES.some(prefix => prefix.startsWith(remaining))) {
          if (!final) this.pending = remaining
          return output
        }
        output += text[start]
        offset = start + 1
        continue
      }
      const end = /\uE201|:walking:/.exec(remaining.slice(prefix.length))
      if (!end) {
        if (!final) this.pending = remaining
        return output
      }
      offset = start + prefix.length + end.index + end[0].length
    }
    return output
  }
}

export function stripUnresolvedCitations(text: string): string {
  const filter = new CitationTextFilter()
  return filter.append(text) + filter.finish()
}
