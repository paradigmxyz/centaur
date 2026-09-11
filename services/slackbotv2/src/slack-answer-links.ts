import { getNodeChildren, isLinkNode, parseMarkdown, toPlainText, type Content } from 'chat'

/**
 * Final-answer fallbacks use Slack's legacy text field (up to 40k), while
 * live streams use native markdown_text (12k per segment). Convert only
 * Markdown links for the text path. Re-serializing the whole answer would
 * change literal code, existing Slack links, mentions and surrounding text.
 */
export function slackAnswerLinks(markdown: string): string {
  const replacements: Array<{ start: number; end: number; text: string }> = []

  function visit(node: Content): void {
    if (isLinkNode(node)) {
      const start = node.position?.start.offset
      const end = node.position?.end.offset
      // Native Slack links and CommonMark autolinks already start with '<'.
      // Only replace explicit [label](target) spans, not bare URLs.
      if (start === undefined || end === undefined || markdown[start] !== '[') return
      if (!/^(https?:\/\/|mailto:)/i.test(node.url)) return
      const label = toPlainText({ type: 'root', children: [{ type: 'paragraph', children: node.children }] })
      const url = escapeSlack(node.url.replaceAll('|', '%7C'))
      replacements.push({ start, end, text: `<${url}|${escapeSlack(label)}>` })
      return
    }
    for (const child of getNodeChildren(node)) visit(child)
  }

  for (const node of parseMarkdown(markdown).children) visit(node)
  let answer = ''
  let offset = 0
  for (const replacement of replacements) {
    answer += markdown.slice(offset, replacement.start) + replacement.text
    offset = replacement.end
  }
  return answer + markdown.slice(offset)
}

function escapeSlack(text: string): string {
  return text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
}
