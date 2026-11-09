import { describe, expect, it } from 'bun:test'
import {
  renderSlackDisplayText,
  slackMessagePromptText,
  slackRichTextMentionsUser
} from '../src/slack-display-text'

const BOT_USER_ID = 'U0ANX3AM5RR'
const MENTION = `<@${BOT_USER_ID}> investigate`

describe('Slack rich-text mentions', () => {
  for (const [name, raw] of [
    ['attachment pretext', { attachments: [{ pretext: MENTION }] }],
    ['attachment fallback', { attachments: [{ fallback: MENTION }] }],
    ['attachment title', { attachments: [{ title: MENTION }] }],
    ['attachment text', { attachments: [{ text: MENTION }] }],
    ['attachment field', { attachments: [{ fields: [{ value: MENTION }] }] }],
    [
      'attachment block',
      { attachments: [{ blocks: [{ type: 'section', text: { type: 'mrkdwn', text: MENTION } }] }] }
    ],
    ['top-level block', { blocks: [{ type: 'section', text: { type: 'mrkdwn', text: MENTION } }] }],
    [
      'Block Kit user element',
      {
        blocks: [
          {
            type: 'rich_text',
            elements: [
              { type: 'rich_text_section', elements: [{ type: 'user', user_id: BOT_USER_ID }] }
            ]
          }
        ]
      }
    ],
    ['labeled mention', { attachments: [{ pretext: `<@${BOT_USER_ID}|centaur> investigate` }] }]
  ] as const) {
    it(`recognizes ${name}`, () => {
      expect(slackRichTextMentionsUser(raw, BOT_USER_ID)).toBe(true)
    })
  }

  it('does not infer a mention from top-level text or plain display text', () => {
    expect(slackRichTextMentionsUser({ text: MENTION }, BOT_USER_ID)).toBe(false)
    expect(slackRichTextMentionsUser({ attachments: [{ pretext: `@${BOT_USER_ID} investigate` }] }, BOT_USER_ID)).toBe(false)
  })

  it('requires the exact configured bot user', () => {
    expect(slackRichTextMentionsUser({ attachments: [{ pretext: MENTION }] }, 'UOTHER')).toBe(false)
    expect(slackRichTextMentionsUser({ attachments: [{ pretext: MENTION }] }, undefined)).toBe(false)
  })
})

describe('Slack display text', () => {
  it('decodes block text escapes once, keeping escaped entities literal', () => {
    const raw = {
      blocks: [
        {
          type: 'section',
          text: { type: 'mrkdwn', text: 'a &lt; b &amp;&amp; Write &amp;lt;br&amp;gt;' }
        }
      ]
    }
    expect(renderSlackDisplayText({ raw, text: '' })).toMatchObject({
      source: 'raw_blocks',
      text: 'a < b && Write &lt;br&gt;'
    })
  })
})

describe('Slack message unfurls', () => {
  const url = 'https://example.slack.com/archives/C0EXAMPLE/p1700000000000100'
  const unfurl = {
    author_name: 'Alex Example',
    fallback: '[January 1st, 2026 9:00 AM] alex: the deploy is green',
    from_url: url,
    is_msg_unfurl: true,
    original_url: url,
    text: 'the deploy is green\nrollout finished at 9am'
  }
  const quoted = [
    `Shared Slack message from Alex Example (${url}):`,
    '> the deploy is green',
    '> rollout finished at 9am'
  ].join('\n')

  it('appends shared message content to typed text', () => {
    const raw = { attachments: [unfurl], text: `<${url}|${url}>` }
    const rendered = renderSlackDisplayText({ raw, text: `cc ${url}` })
    expect(rendered).toMatchObject({
      source: 'text_with_unfurls',
      text: `cc ${url}\n\n${quoted}`
    })
    const promptText = slackMessagePromptText({
      displayText: rendered.text,
      displayTextSource: rendered.source,
      text: `cc ${url}`
    })
    expect(promptText).toBe(`cc ${url}\n\n${quoted}`)
  })

  it('appends shared message content to block text', () => {
    const raw = {
      attachments: [unfurl],
      blocks: [{ type: 'section', text: { type: 'mrkdwn', text: 'see this' } }]
    }
    expect(renderSlackDisplayText({ raw, text: '' })).toMatchObject({
      source: 'raw_blocks',
      text: `see this\n\n${quoted}`
    })
  })

  it('falls back to the unfurl fallback text and dedupes repeated records', () => {
    const fallbackOnly = { ...unfurl, author_name: undefined, text: undefined }
    const raw = { attachments: [fallbackOnly], event: { attachments: [{ ...fallbackOnly }] } }
    expect(renderSlackDisplayText({ raw, text: 'look' }).text).toBe(
      `look\n\nShared Slack message (${url}):\n> [January 1st, 2026 9:00 AM] alex: the deploy is green`
    )
  })

  it('ignores non-message unfurls', () => {
    const raw = {
      attachments: [{ from_url: 'https://example.test', text: 'a web page', title: 'Page' }]
    }
    expect(renderSlackDisplayText({ raw, text: 'link' })).toMatchObject({ source: 'text', text: 'link' })
  })
})
