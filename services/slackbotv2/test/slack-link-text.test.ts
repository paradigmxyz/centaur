import { describe, expect, it } from 'bun:test'
import { qualifySlackMessageLinks, SlackLinkTextFilter } from '../src/slack-link-text'

const workspace = 'https://acme.enterprise.slack.com/'
const link = 'https://slack.com/archives/C1234567890/p1791832707371619?thread_ts=1791832000.000001&cid=C1234567890'
const qualified = link.replace('https://slack.com/', workspace)

describe('Slack message links', () => {
  it('qualifies generic archive links and preserves labels and thread parameters', () => {
    expect(qualifySlackMessageLinks(`[Discussion](${link}) and <${link}|source>`, workspace))
      .toBe(`[Discussion](${qualified}) and <${qualified}|source>`)
  })

  it('qualifies links at every delta boundary', () => {
    for (let split = 1; split < link.length; split++) {
      const filter = new SlackLinkTextFilter(workspace)
      expect(filter.append(link.slice(0, split)) + filter.append(link.slice(split)) + filter.finish())
        .toBe(qualified)
    }
    const filter = new SlackLinkTextFilter(workspace)
    expect([...link].map(character => filter.append(character)).join('') + filter.finish())
      .toBe(qualified)
  })

  it('preserves qualified links, other domains, and non-archive Slack URLs', () => {
    const text = '[Other](https://other.slack.com/archives/C123/p123) ' +
      'https://slack.com.evil.example/archives/C123/p123 https://slack.com/help'
    expect(qualifySlackMessageLinks(text, workspace)).toBe(text)
  })

  it('preserves partial prefixes at completion', () => {
    const prefix = 'https://slack.com/archives/'
    for (let length = 1; length < prefix.length; length++) {
      const text = `unfinished ${prefix.slice(0, length)}`
      expect(qualifySlackMessageLinks(text, workspace)).toBe(text)
    }
  })

  it('leaves URLs unchanged without a valid HTTPS workspace URL', () => {
    for (const url of [undefined, '', 'invalid', 'http://acme.slack.com', 'https://evil.example', 'https://slack.com.evil.example']) {
      expect(qualifySlackMessageLinks(link, url)).toBe(link)
    }
  })

  it('supports standard workspace domains', () => {
    expect(qualifySlackMessageLinks(link, 'https://acme.slack.com/'))
      .toBe(link.replace('https://slack.com/', 'https://acme.slack.com/'))
  })
})
