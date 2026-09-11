import { describe, expect, it } from 'bun:test'
import { slackAnswerLinks } from '../src/slack-answer-links'

describe('slackAnswerLinks', () => {
  it('renders a linked completion result for the legacy Slack text field', () => {
    expect(slackAnswerLinks('Marked your [Notion fixes to-do](https://example.com/task) as Done.'))
      .toBe('Marked your <https://example.com/task|Notion fixes to-do> as Done.')
  })

  it('uses parsed link boundaries for punctuation and formatted labels', () => {
    expect(slackAnswerLinks('Read [the **spec**](https://example.com/spec_(v2)), then [notes](https://example.com/notes "Notes").'))
      .toBe('Read <https://example.com/spec_(v2)|the spec>, then <https://example.com/notes|notes>.')
  })

  it('preserves existing Slack links, mentions, bare URLs and surrounding text', () => {
    const prefix = '<@U123> <#C123|channel> <https://example.com/a|Existing> https://example.com/b\n'
    const answer = prefix + '[New](https://example.com/c)'
    expect(slackAnswerLinks(answer)).toBe(prefix + '<https://example.com/c|New>')
    expect(slackAnswerLinks(slackAnswerLinks(answer))).toBe(slackAnswerLinks(answer))
  })

  it('leaves inline and fenced code literal', () => {
    const code = '`[inline](https://example.com)`\n\n```md\n[fenced](https://example.com)\n```\n\n'
    expect(slackAnswerLinks(code + '[Visible](https://example.com)'))
      .toBe(code + '<https://example.com|Visible>')
  })

  it('escapes control characters in labels and URLs', () => {
    expect(slackAnswerLinks('[A & B](https://example.com/?a=1&b=2) [mail](mailto:person@example.com)'))
      .toBe('<https://example.com/?a=1&amp;b=2|A &amp; B> <mailto:person@example.com|mail>')
  })

  it('preserves malformed links and unsupported destinations', () => {
    const text = '[incomplete](https://example.com [local](./file.md) [unsafe](javascript:alert)'
    expect(slackAnswerLinks(text)).toBe(text)
  })

  it('retains answers longer than the native Markdown field limit', () => {
    const prefix = 'x'.repeat(13_000) + '\n'
    expect(slackAnswerLinks(prefix + '[Task](https://example.com/task)'))
      .toBe(prefix + '<https://example.com/task|Task>')
  })
})
