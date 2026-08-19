import { describe, expect, it } from 'bun:test'
import { slackRichTextMentionsUser } from '../src/slack-display-text'

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
