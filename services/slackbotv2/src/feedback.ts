import type { ActionEvent, Logger, ModalSubmitEvent } from 'chat'

/** action_id on the feedback_buttons element of the final answer message. */
export const FEEDBACK_ACTION_ID = 'centaur_feedback'
/** callback_id of the free-text details modal opened from a 👎. */
export const FEEDBACK_MODAL_CALLBACK_ID = 'centaur_feedback_submit'
/** Slack caps plain_text_input max_length at 3000 characters. */
const FEEDBACK_MESSAGE_MAX_LENGTH = 3000
/** Slack expects interaction acks within 3 seconds. */
const FEEDBACK_API_TIMEOUT_MS = 2500

type FeedbackChat = {
  onAction(actionId: string, handler: (event: ActionEvent) => Promise<void>): void
  onModalSubmit(callbackId: string, handler: (event: ModalSubmitEvent) => Promise<void>): void
}

export type FeedbackOptions = {
  apiKey?: string
  apiUrl: string
  logger: Logger
}

type FeedbackContext = {
  channel_id?: string
  message_ts?: string
  team_id?: string
  thread_ts?: string
}

/**
 * Blocks appended to the final streamed answer via Slack's `chat.stopStream`
 * (`StreamingPlan.endWith` / adapter `stopBlocks`). `feedback_buttons` renders
 * as Slack's native subtle 👍/👎 affordance on the message — no extra OAuth
 * scope and no app reinstall required, unlike shortcuts.
 */
export function feedbackEndBlocks(): unknown[] {
  return [
    {
      type: 'context_actions',
      elements: [
        {
          type: 'feedback_buttons',
          action_id: FEEDBACK_ACTION_ID,
          positive_button: {
            text: { type: 'plain_text', text: 'Good response' },
            accessibility_label: 'Mark this response as good',
            value: 'positive'
          },
          negative_button: {
            text: { type: 'plain_text', text: 'Bad response' },
            accessibility_label: 'Mark this response as bad and add details',
            value: 'negative'
          }
        }
      ]
    }
  ]
}

/**
 * Wires the feedback flow: 👍 stores a one-tap positive record, 👎 opens a
 * free-text modal whose submission is stored via the Centaur feedback API.
 */
export function registerFeedbackHandlers(chat: FeedbackChat, options: FeedbackOptions): void {
  chat.onAction(FEEDBACK_ACTION_ID, async event => {
    const sentiment = event.value === 'negative' ? 'negative' : 'positive'
    const context = feedbackContext(event)
    if (sentiment === 'negative' && (await openFeedbackModal(event, context, options))) return
    // Positive feedback is one tap; a negative tap only lands here when the
    // modal could not be opened, so the bare sentiment is still recorded.
    await storeFeedback(options, {
      source: 'slackbotv2_feedback_button',
      message: sentiment === 'positive' ? '+1' : '-1',
      user_id: event.user.userId,
      channel_id: context.channel_id,
      thread_ts: context.thread_ts,
      metadata: { sentiment, slack: context }
    })
  })

  chat.onModalSubmit(FEEDBACK_MODAL_CALLBACK_ID, async event => {
    const message = event.values.message?.trim()
    if (!message) return
    const context = parseFeedbackContext(event.privateMetadata)
    await storeFeedback(options, {
      source: 'slackbotv2_feedback_modal',
      message,
      user_id: event.user.userId,
      channel_id: context.channel_id,
      thread_ts: context.thread_ts,
      metadata: { sentiment: 'negative', slack: context }
    })
  })
}

async function openFeedbackModal(
  event: ActionEvent,
  context: FeedbackContext,
  options: FeedbackOptions
): Promise<boolean> {
  try {
    const result = await event.openModal({
      type: 'modal',
      callbackId: FEEDBACK_MODAL_CALLBACK_ID,
      title: 'Feedback',
      submitLabel: 'Send',
      privateMetadata: JSON.stringify(context),
      children: [
        {
          type: 'text_input',
          id: 'message',
          label: 'What went wrong?',
          multiline: true,
          maxLength: FEEDBACK_MESSAGE_MAX_LENGTH,
          placeholder: 'Tell us what to improve'
        }
      ]
    })
    return Boolean(result)
  } catch (error) {
    options.logger.warn('slackbotv2_feedback_modal_open_failed', { error: String(error) })
    return false
  }
}

function feedbackContext(event: ActionEvent): FeedbackContext {
  const raw = (event.raw ?? {}) as {
    channel?: { id?: string }
    container?: { channel_id?: string; message_ts?: string; thread_ts?: string }
    message?: { thread_ts?: string; ts?: string }
    team?: { id?: string }
  }
  const messageTs = raw.message?.ts ?? raw.container?.message_ts
  return pruneContext({
    channel_id: raw.channel?.id ?? raw.container?.channel_id,
    message_ts: messageTs,
    team_id: raw.team?.id,
    thread_ts: raw.message?.thread_ts ?? raw.container?.thread_ts ?? messageTs
  })
}

function parseFeedbackContext(privateMetadata: string | undefined): FeedbackContext {
  if (!privateMetadata) return {}
  try {
    const parsed: unknown = JSON.parse(privateMetadata)
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {}
    return pruneContext(
      Object.fromEntries(
        Object.entries(parsed).filter(([, value]) => typeof value === 'string')
      ) as FeedbackContext
    )
  } catch {
    return {}
  }
}

function pruneContext(context: FeedbackContext): FeedbackContext {
  return Object.fromEntries(
    Object.entries(context).filter(([, value]) => Boolean(value))
  ) as FeedbackContext
}

async function storeFeedback(
  options: FeedbackOptions,
  body: {
    channel_id?: string
    message: string
    metadata: Record<string, unknown>
    source: string
    thread_ts?: string
    user_id?: string
  }
): Promise<void> {
  const apiKey = options.apiKey ?? process.env.SLACKBOT_API_KEY ?? process.env.CENTAUR_API_KEY
  try {
    const response = await fetch(new URL('/api/feedback', options.apiUrl), {
      body: JSON.stringify(body),
      headers: {
        'content-type': 'application/json',
        ...(apiKey ? { authorization: `Bearer ${apiKey}` } : {})
      },
      method: 'POST',
      signal: AbortSignal.timeout(FEEDBACK_API_TIMEOUT_MS)
    })
    if (!response.ok) {
      throw new Error(`feedback API returned ${response.status}`)
    }
  } catch (error) {
    options.logger.warn('slackbotv2_feedback_store_failed', {
      error: String(error),
      source: body.source
    })
  }
}
