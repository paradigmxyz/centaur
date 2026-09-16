import { describe, expect, it } from 'bun:test'
import { renderSlackDisplayText, slackMessagePromptText, slackRichTextMentionsUser } from '../src/slack-display-text'

describe('Slack bot message bodies', () => {
  const summary = 'Firing: ReplicaMismatch'
  const command = '<@UBOT> $alert-investigation'
  const raw = {
    blocks: [
      { type: 'header', text: { type: 'plain_text', text: summary } },
      { type: 'section', text: { type: 'mrkdwn', text: '*Cluster:* `test-cluster`' } },
      { type: 'section', text: { type: 'mrkdwn', text: command } }
    ]
  }

  it('includes the visible bot body and command despite a nonempty notification summary', () => {
    const display = renderSlackDisplayText({ isBot: true, raw, text: summary })
    expect(display.source).toBe('raw_blocks')
    expect(display.text).toBe(`${summary}\n*Cluster:* \`test-cluster\`\n@UBOT $alert-investigation`)
    expect(slackMessagePromptText({
      displayText: display.text, displayTextSource: display.source, text: summary
    })).toBe(display.text)
  })

  it('retains a distinct bot summary not repeated in its blocks', () => {
    const display = renderSlackDisplayText({ isBot: true, raw, text: 'Additional alert context' })
    expect(display.text).toStartWith(`Additional alert context\n${summary}\n`)
  })

  it('preserves human-authored text rather than replacing it with rich content', () => {
    const text = 'Please review this message\n\nKeep these paragraphs.'
    expect(renderSlackDisplayText({ isBot: false, raw, text })).toMatchObject({ source: 'text', text })
    expect(renderSlackDisplayText({ raw, text })).toMatchObject({ source: 'text', text })
  })

  it('keeps bot notification text when blocks contain no readable content', () => {
    expect(renderSlackDisplayText({ isBot: true, raw: { blocks: [{ type: 'divider' }] }, text: summary }))
      .toMatchObject({ source: 'text', text: summary })
  })

  it('does not promote attachment unfurls into a nonempty bot message', () => {
    const display = renderSlackDisplayText({
      isBot: true, text: summary,
      raw: { attachments: [{ is_msg_unfurl: true, blocks: raw.blocks }] }
    })
    expect(display).toMatchObject({ source: 'text', text: summary })
  })
})

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
