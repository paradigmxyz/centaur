// The vocabulary e2e scenarios are written in. Tests act through the same
// doors a user and an operator have: Slack for input and visible output, the
// durable session tables for what the control plane recorded, the scripted
// model server for what the harness actually asked its provider, and api-rs
// and Kubernetes for operator actions and faults.
//
//   const thread = await slack.mention('--codex hi', model.says('Hello.'))
//   const turn = await thread.nextTurn()
//   expect(turn.reply).toBe('Hello.')
import { randomUUID } from 'node:crypto'
import { afterAll } from 'bun:test'
import { SQL } from 'bun'
import { BOT, CHANNEL, USER, type SlackUser } from '../fakes/slack-fixture'

export {
  CHANNEL,
  DEFAULTS_CHANNEL,
  EXTERNAL_TEAM,
  EXTERNAL_USER,
  TEAM,
  USER,
  USER_B,
  type SlackUser
} from '../fakes/slack-fixture'
export type Channel = { id: string; name: string }

/** A provider error, as Anthropic's HTTP error or OpenAI's failed response carries it. */
export type ProviderError = { status: number; type: string; code?: string; message: string }

/**
 * A scripted model answer, matched by a token placed in the user's message.
 * `run` first has the harness run a shell command through its shell tool;
 * `cell` first has it run a Code Mode cell through `centaur_execute`.
 */
export type Script = { token: string; text: string; delayMs?: number; error?: ProviderError; run?: string; cell?: string }

export type Execution = { execution_id: string; status: string; error: string | null }

/** A provider request as the model server received it, normalized across providers. */
export type ModelRequest = {
  provider: 'openai' | 'anthropic'
  model?: string
  host?: string
  /** The credential header as the provider received it, after iron-proxy. */
  credential?: string
  /** The ChatGPT workspace the request was sent for, after iron-proxy. */
  account?: string
  /** Reasoning effort the harness asked for. */
  effort?: string
  /**
   * What the user wrote this turn: the last content block of the newest user
   * message. Earlier blocks carry session context and, after a harness
   * switch, the re-fed thread transcript.
   */
  userText: string
  /** Every text block of the newest user message: the context slackbotv2 added, then what the user wrote. */
  userMessage: string
  /** Earlier assistant turns in the conversation the model was asked to continue. */
  assistantTurns: string[]
  /** Everything the model was given, as text: system instructions and the whole conversation. */
  prompt: string
  body: any
}

/** The sandbox that ran a turn, and how the control plane obtained it. */
export type Sandbox = { id: string; harness: string; source: string }

export type Turn = {
  /** Text of the turn's one Slack reply. */
  reply: string
  /** Text of the reply's context line: response metadata and notices. */
  context: string
  execution: Execution
  sandbox: Sandbox
  /** The provider request answered by this turn's script, if it had one. */
  request?: ModelRequest
}

/** A `nanocode` turn: it has no execution row or sandbox, only the brain's event stream. */
export type BrainTurn = {
  /** Text of the turn's one Slack reply. */
  reply: string
  execution: Execution
  /** The api-rs `session.first_token` latency the brain reported, in milliseconds. */
  firstTokenMs: number
}

export type SlackMessage = {
  ts: string
  text?: string
  user?: string
  bot_id?: string
  thread_ts?: string
  streaming?: boolean
  blocks?: Array<{ type?: string; elements?: Array<{ text?: string; action_id?: string; value?: string }> }>
  reactions?: Array<{ name: string; users: string[] }>
}

const slackUrl = required('E2E_SLACK_URL')
const modelUrl = required('E2E_MODEL_URL')
/** The session database, for the durable rows a turn leaves behind. */
export const db = new SQL(required('E2E_DATABASE_URL'))
const ironControlDb = new SQL(required('E2E_IRON_CONTROL_DATABASE_URL'))
const apiUrl = required('E2E_API_URL')
const apiKey = required('E2E_API_KEY')
const consoleUrl = required('E2E_CONSOLE_URL')
const consoleApiKey = required('E2E_CONSOLE_API_KEY')
const consoleAdminEmail = required('E2E_CONSOLE_ADMIN_EMAIL')
const consoleAdminPassword = required('E2E_CONSOLE_ADMIN_PASSWORD')
const logDir = required('E2E_LOG_DIR')
const kindNode = required('E2E_KIND_NODE')
const workflowDir = required('E2E_WORKFLOW_DIR')
const namespace = 'centaur'
const sshNamespace = 'centaur-e2e-ssh'
const release = 'centaur'

/** How the stack's Codex and Claude Code authenticate: api_key, or access_token (subscriptions). */
export const authMode = required('E2E_AUTH_MODE')
/**
 * The credential iron-proxy injects for each provider, as the provider
 * receives it: an API key, or a subscription access token in access_token mode.
 */
export const providerCredentials: Record<ModelRequest['provider'], string> = {
  openai: required('E2E_OPENAI_CREDENTIAL'),
  anthropic: required('E2E_ANTHROPIC_CREDENTIAL')
}
/** The ChatGPT workspace iron-proxy routes Codex to; only subscription requests carry one. */
export const chatgptAccountId = process.env.E2E_CHATGPT_ACCOUNT_ID
export const turnTimeoutMs = Number(process.env.E2E_TURN_TIMEOUT_MS ?? 300_000)
/** How long a finished execution may take to show up in Slack. */
const renderTimeoutMs = 60_000

export async function executionOutput(executionId: string): Promise<any[]> {
  const events = await db`
    select payload from session_events
    where execution_id = ${executionId} and event_type = 'session.output.line'
    order by event_id`
  return events.map(({ payload }: { payload: string }) => JSON.parse(payload))
}

export const model = {
  /**
   * Scripts the model's answer to the message this is attached to. `delayMs`
   * holds the answer back, keeping the turn in flight.
   */
  says(text: string, options: { delayMs?: number } = {}): Script {
    return { token: `e2e-${randomUUID().slice(0, 8)}`, text, ...options }
  },
  /**
   * Scripts the model to run a shell command through the harness's shell tool
   * (OpenAI harnesses), then answer with `text` once the command finishes.
   */
  runs(command: string, text = 'Ran it.', options: { delayMs?: number } = {}): Script {
    return { token: `e2e-${randomUUID().slice(0, 8)}`, text, run: command, ...options }
  },
  /**
   * Scripts the model to run a Code Mode cell through `centaur_execute`
   * (nanocode turns in the brain), then answer with `text`.
   */
  runsCell(code: string, text = 'Ran the cell.'): Script {
    return { token: `e2e-${randomUUID().slice(0, 8)}`, text, cell: code }
  },
  /** What the model received as the output of a script's tool call, verbatim. */
  async toolOutput(script: Script): Promise<string> {
    for (const { body } of await modelRequests(script.token)) {
      const outputs = (Array.isArray(body.input) ? body.input : [])
        .filter((item: any) => String(item?.type ?? '').endsWith('_call_output'))
        .map((item: any) => (typeof item.output === 'string' ? item.output : contentText(item.output)))
      if (outputs.length > 0) return outputs.join('\n')
    }
    throw new Error(`the model received no tool output for script ${script.token}`)
  },
  /** What the harness's shell tool returned for a `runs` script's command, as the model received it. */
  async commandOutput(script: Script): Promise<string> {
    for (const { body } of await modelRequests(script.token)) {
      const outputs = (Array.isArray(body.input) ? body.input : [])
        .filter((item: any) => String(item?.type ?? '').endsWith('_call_output'))
        .map((item: any) => (typeof item.output === 'string' ? item.output : contentText(item.output)))
        .map(commandText)
      if (outputs.length > 0) return outputs.join('\n')
    }
    throw new Error(`the harness returned no command output for script ${script.token}`)
  },
  /**
   * The X-E2E-Credential header of each request a command made to
   * https://<provider host>/_e2e/echo/<match>, oldest first.
   */
  async echoes(match: string): Promise<Array<string | undefined>> {
    const response = await fetch(`${modelUrl}/_e2e/echoes?match=${encodeURIComponent(match)}`)
    const echoes = (await response.json()) as Array<{ credential?: string }>
    return echoes.map(echo => echo.credential)
  },
  /** Scripts the provider to fail the request instead of answering it. */
  fails(error: ProviderError): Script {
    return { token: `e2e-${randomUUID().slice(0, 8)}`, text: '', error }
  },
  /** Registers a script for a prompt the test sends some other way than Slack. */
  async register(script: Script): Promise<void> {
    await registerScript(script)
  },
  /** Provider requests the script answered, oldest first. */
  async requests(script: Script): Promise<ModelRequest[]> {
    return modelRequests(script.token)
  }
}

/** A JSON API called with an operator's bearer key. */
function operatorApi(baseUrl: string, key: string) {
  const client = {
    /** Calls the API; returns the status and JSON body. */
    async request(method: string, path: string, body?: unknown): Promise<{ status: number; body: any }> {
      const response = await fetch(`${baseUrl}${path}`, {
        method,
        headers: { authorization: `Bearer ${key}`, 'content-type': 'application/json' },
        ...(body === undefined ? {} : { body: JSON.stringify(body) })
      })
      const text = await response.text()
      let payload: unknown = text
      try {
        payload = text ? JSON.parse(text) : undefined
      } catch {
        // Rejections from request extractors are plain text.
      }
      return { status: response.status, body: payload }
    },
    /** Like request, but fails unless the API answers 2xx. */
    async ok(method: string, path: string, body?: unknown): Promise<any> {
      const { status, body: payload } = await client.request(method, path, body)
      if (status < 200 || status > 299) throw new Error(`${method} ${path} failed: ${status} ${JSON.stringify(payload)}`)
      return payload
    }
  }
  return client
}

/** api-rs, called as an operator with the e2e admin key. */
export const api = {
  ...operatorApi(apiUrl, apiKey),
  /** Pauses the thread's sandbox now, as its idle timeout would; returns whether it paused. */
  async pause(thread: Thread): Promise<boolean> {
    const result = await api.ok('POST', `/api/session/${encodeURIComponent(thread.key)}/pause`)
    return result.paused
  }
}

/**
 * Stops the file's idle sandboxes once all its tests finish. Call it at the top
 * level of every test file.
 *
 * A thread keeps its sandbox and iron-proxy pod until it idles out (hours),
 * so without this a run holds two pods for every thread it opened, and the
 * single kind node runs out of pods. Bun runs files one at a time, so when a
 * file's tests are done, every idle sandbox is one of its own finished
 * threads. api-rs leaves sandboxes with a running execution alone.
 */
export function releaseSandboxesAfterAll(): void {
  afterAll(async () => {
    const report = await api.ok('POST', '/api/sandboxes/drain')
    if (report.failed.length > 0) throw new Error(`sandbox release failed: ${JSON.stringify(report.failed)}`)
    console.log(`released ${report.stopped_count} idle sandboxes; ${report.busy_count} busy`)
  }, 60_000)
}

export const cluster = {
  /** Kills a sandbox's pod without a graceful shutdown. */
  async killSandbox(sandboxId: string): Promise<void> {
    await kubectl({}, 'delete', 'pod', sandboxId, '--grace-period=0', '--force')
  },
  /** Restarts a release component and waits for the new pods to be ready. */
  async restart(component: string): Promise<void> {
    // The new pods replace the old ones and their logs; keep them for failures earlier in the run.
    const logs = await kubectl({}, 'logs', `deploy/${release}-centaur-${component}`, '--tail=-1')
      .catch(error => String(error))
    await Bun.write(`${logDir}/${component}-before-restart-${Date.now()}.log`, logs)
    await kubectl({}, 'rollout', 'restart', `deploy/${release}-centaur-${component}`)
    await kubectl({}, 'rollout', 'status', `deploy/${release}-centaur-${component}`, '--timeout=180s')
  },
  /** What a sandbox's iron-proxy reports applying from proxy-sync, from its management API. */
  async proxyStatus(sandboxId: string): Promise<{ config_hash: string; principal_id: string; last_sync_at: string }> {
    const pod = await kubectl(
      {},
      'get', 'pods', '-l', `centaur.ai/iron-proxy=true,centaur.ai/sandbox-id=${sandboxId}`,
      '-o', 'jsonpath={.items[0].metadata.name}'
    )
    if (!pod) throw new Error(`sandbox ${sandboxId} has no iron-proxy pod`)
    const status = await kubectl(
      {},
      'exec', pod, '-c', 'iron-proxy', '--', 'sh', '-c',
      'curl -fsS -H "Authorization: Bearer $IRON_MANAGEMENT_API_KEY" "http://127.0.0.1:${IRON_MANAGEMENT_LISTEN##*:}/v1/status"'
    )
    return JSON.parse(status)
  },
  /** Kubernetes objects api-rs created for a sandbox and has not removed. */
  async sandboxResources(sandboxId: string): Promise<string[]> {
    const out = await kubectl(
      {},
      'get', 'sandboxes.agents.x-k8s.io,pods,services,networkpolicies',
      '-l', `centaur.ai/sandbox-id=${sandboxId}`, '-o', 'name'
    )
    return out.split('\n').filter(Boolean)
  }
}

/**
 * The remote development environment behind the e2e SSH gateway
 * (e2e/infra/ssh-gateway.yaml), as its owner sees it.
 */
export const sshGateway = {
  /** The WSS URL the deployment's SSH target policy allows. */
  url: 'wss://workspace.ssh-gateway.test/events/events',
  /** The private key the gateway authorizes for its `dev` user. */
  privateKey: required('E2E_SSH_PRIVATE_KEY'),
  /** Runs a shell script on the remote host as root; the workspace is /workspace, owned by `dev`. */
  async exec(script: string): Promise<string> {
    return kubectl({ namespace: sshNamespace }, 'exec', 'deploy/ssh-gateway', '-c', 'sshd', '--', 'sh', '-c', script)
  }
}

type MentionOptions = {
  channel?: Channel
  as?: SlackUser
  /** Mentions the bot in an existing thread instead of starting one. */
  threadTs?: string
}

export const slack = {
  /** Starts a thread with a channel message that mentions the bot, by USER unless `as` says otherwise. */
  async mention(text: string, script?: Script, options: MentionOptions = {}): Promise<Thread> {
    const channel = options.channel ?? CHANNEL
    const ts = await postMention(channel, options.as ?? USER, text, script, options.threadTs)
    return new Thread(channel, options.threadTs ?? ts, script, { trigger: ts })
  },
  /** Posts a message that does not mention the bot, starting a thread or replying in one. */
  async post(text: string, options: { channel?: Channel; as?: SlackUser; threadTs?: string } = {}): Promise<string> {
    const posted = await slackApi<{ ts: string }>(options.as ?? USER, 'chat.postMessage', {
      channel: (options.channel ?? CHANNEL).id,
      text,
      ...(options.threadTs ? { thread_ts: options.threadTs } : {})
    })
    return posted.ts
  },
  /** Opens the user's DM with the bot and sends it a message; a DM needs no mention. */
  async dm(text: string, script?: Script, options: { as?: SlackUser } = {}): Promise<Thread> {
    const user = options.as ?? USER
    const { channel } = await slackApi<{ channel: { id: string } }>(user, 'conversations.open', { users: user.id })
    const dm = { id: channel.id, name: `dm-${user.name}` }
    const ts = await postMention(dm, user, text, script, undefined, { dm: true })
    return new Thread(dm, ts, script, { dm: true })
  },
  /** A message as Slack holds it, with its reactions. */
  async message(channel: Channel, ts: string): Promise<SlackMessage> {
    const { message } = await slackApi<{ message: SlackMessage }>(USER, 'reactions.get', { channel: channel.id, timestamp: ts })
    return message
  },
  /** A channel's top-level messages, newest first. */
  async history(channel: Channel): Promise<SlackMessage[]> {
    const { messages } = await slackApi<{ messages: SlackMessage[] }>(USER, 'conversations.history', { channel: channel.id })
    return messages
  },
  /** Ephemeral messages a user was shown, oldest first. */
  async ephemeral(user: SlackUser): Promise<Array<{ channel: string; text: string }>> {
    const response = await fetch(`${slackUrl}/_e2e/ephemeral?user=${encodeURIComponent(user.id)}`)
    return response.json()
  },
  /**
   * Clicks a block element on a message, as `as` (USER by default). `value`
   * forges the element's value; `actionTs` repeats an earlier click's delivery.
   */
  async click(
    channel: Channel,
    ts: string,
    actionId: string,
    options: { as?: SlackUser; value?: string; actionTs?: string } = {}
  ): Promise<{ status: number; actionTs: string }> {
    const response = await fetch(`${slackUrl}/_e2e/actions`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        user: (options.as ?? USER).id,
        channel: channel.id,
        message_ts: ts,
        action_id: actionId,
        value: options.value,
        action_ts: options.actionTs
      })
    })
    const result = (await response.json()) as { ok: boolean; error?: string; status: number; action_ts: string }
    if (!result.ok) throw new Error(`clicking ${actionId} failed: ${result.error}`)
    return { status: result.status, actionTs: result.action_ts }
  }
}

export const workflows = {
  /** The payload of the latest workflow event with this name, if one was emitted. */
  async event(name: string): Promise<unknown> {
    const [row] = await db`select payload from absurd.e_centaur_workflows where event_name = ${name}`
    return row?.payload
  },
  /**
   * Deploys a workflow file the way an operator updates a workflow repo: into
   * the node directory the workflow discovery service and every workflow-host
   * sandbox mount. The rename makes the file appear whole to a discovery pass
   * that is reading the directory.
   */
  async install(name: string, source: string): Promise<void> {
    const path = `${workflowDir}/${name}.py`
    await docker(source, 'exec', '-i', kindNode, 'sh', '-c', `cat > '${path}.tmp' && mv '${path}.tmp' '${path}'`)
  },
  /** Deletes a workflow file installed with `install`. */
  async uninstall(name: string): Promise<void> {
    await docker(undefined, 'exec', kindNode, 'rm', '-f', `${workflowDir}/${name}.py`)
  },
  /** The names of the steps a run has checkpointed so far. */
  async checkpoints(taskId: string, runId: string): Promise<string[]> {
    const rows = await db`
      select checkpoint_name from absurd.get_task_checkpoint_states('centaur_workflows', ${taskId}::uuid, ${runId}::uuid)`
    return rows.map((row: { checkpoint_name: string }) => row.checkpoint_name)
  }
}

/** A principal as iron-control holds it: who a proxy's credentials act for. */
export type Principal = {
  foreignId: string
  name: string
  kind: string
  slackUserId: string | null
  slackTeamId: string | null
  slackEmail: string | null
}

/**
 * What the control plane registered in iron-control, the source of every
 * sandbox proxy's credentials and grants.
 */
export const ironControl = {
  /** The console API, called as an operator with the key api-rs administers it with. */
  api: operatorApi(consoleUrl, consoleApiKey),
  /** The principals a sandbox's proxy acts for: the conversation, and the requester if one is bound. */
  async proxy(sandboxId: string): Promise<{ principal: Principal; requester: Principal | null }> {
    const [proxy] = await ironControlDb`
      select principal_id, requester_principal_id from proxies where name = ${sandboxId}`
    if (!proxy) throw new Error(`iron-control has no proxy for sandbox ${sandboxId}`)
    const [principal, requester] = await Promise.all(
      [proxy.principal_id, proxy.requester_principal_id].map(async id => {
        if (id === null) return null
        const [row]: Principal[] = await ironControlDb`
          select foreign_id as "foreignId", name, kind, slack_user_id as "slackUserId",
            slack_team_id as "slackTeamId", slack_email as "slackEmail"
          from principals where id = ${id}`
        return row ?? null
      })
    )
    if (!principal) throw new Error(`proxy for sandbox ${sandboxId} has no principal`)
    return { principal, requester: requester ?? null }
  },
  /** The Console id (`prn_…`) of a Slack channel's principal. */
  async slackChannelPrincipalId(channelId: string): Promise<string> {
    const [row] = await ironControlDb`
      select foreign_id as "foreignId" from principals where slack_channel_id = ${channelId}`
    if (!row) throw new Error(`iron-control has no principal for channel ${channelId}`)
    const { data } = await this.api.ok('GET', `/api/v1/principals/lookup/${encodeURIComponent(row.foreignId)}`)
    return data.id
  },
  /** The principal for a Slack user, if iron-control has one. */
  async slackUser(user: SlackUser): Promise<Principal | undefined> {
    const [row]: Principal[] = await ironControlDb`
      select foreign_id as "foreignId", name, kind, slack_user_id as "slackUserId",
        slack_team_id as "slackTeamId", slack_email as "slackEmail"
      from principals where slack_user_id = ${user.id}`
    return row
  }
}

/** A Console scheduled task, with the columns api-rs writes when it runs it. */
export type ScheduledTask = {
  id: number
  oid: string
  enabled: boolean
  nextRunAt: Date | null
  lastEnqueuedAt: Date | null
  lastRunAt: Date | null
  lastRunId: string | null
  lastError: string | null
}

/** What happened when a Console user pressed "Run now". */
export type ScheduledTaskRun = { started: true; runId: string } | { started: false; status: number; message: string }

/**
 * Console's scheduled tasks. Console stores them; api-rs runs them, on their
 * schedule and when a user runs one now.
 */
export const scheduledTasks = {
  /**
   * Creates a task as `as`'s Console account would, delivering to `channel`,
   * which the bot is in. A null `cron` makes a manual task.
   */
  async create(options: { prompt: string; channel: Channel; cron: string | null; as?: SlackUser }): Promise<ScheduledTask> {
    const user = options.as ?? USER
    const name = `e2e-${randomUUID().slice(0, 8)}`
    // The author's Console account, Slack identity, and the channel the bot
    // is in, as Slack sign-in and the channel catalog would have left them.
    // Concurrent tests share them, so every write is an idempotent upsert.
    const digest = await Bun.password.hash(AUTHOR_PASSWORD, { algorithm: 'bcrypt', cost: 4 })
    const [account] = await ironControlDb`
      insert into users (email, name, status, password_digest, created_at, updated_at)
      values (${user.email}, ${user.displayName}, 'active', ${digest},
              now() at time zone 'UTC', now() at time zone 'UTC')
      on conflict (email) do update set status = 'active', password_digest = excluded.password_digest,
        updated_at = excluded.updated_at
      returning id`
    await ironControlDb`
      insert into user_identities (user_id, provider, subject, team_id, created_at, updated_at)
      values (${account.id}, 'slack', ${user.id}, ${user.teamId}, now() at time zone 'UTC', now() at time zone 'UTC')
      on conflict (provider, subject) do nothing`
    await ironControlDb`
      insert into slack_bot_channels (team_id, channel_id, bot_user_id, name, private, active, archived,
                                      created_at, updated_at)
      values (${user.teamId}, ${options.channel.id}, ${BOT.userId}, ${options.channel.name}, false, true, false,
              now() at time zone 'UTC', now() at time zone 'UTC')
      on conflict (team_id, channel_id) do update set bot_user_id = excluded.bot_user_id, name = excluded.name,
        private = false, active = true, archived = false, updated_at = excluded.updated_at`
    const session = await consoleSession(user.email, AUTHOR_PASSWORD)
    const { data } = await session.ok('POST', '/api/v1/console/scheduled_tasks', {
      data: {
        name,
        prompt: options.prompt,
        delivery_mode: 'channel',
        delivery_channel: options.channel.id,
        schedule_preset: options.cron ? 'cron' : 'manual',
        cron_expression: options.cron ?? ''
      }
    })
    const [row] = await ironControlDb`select id from scheduled_tasks where name = ${name}`
    const id = Number(row.id)
    taskAuthors.set(id, { oid: data.id, email: user.email })
    return scheduledTasks.get(id)
  },
  async get(id: number): Promise<ScheduledTask> {
    // Console stores UTC times without a zone.
    const [row] = await ironControlDb`
      select id, enabled, next_run_at at time zone 'UTC' as next_run_at,
        last_enqueued_at at time zone 'UTC' as last_enqueued_at,
        last_run_at at time zone 'UTC' as last_run_at, last_run_id, last_error
      from scheduled_tasks where id = ${id}`
    if (!row) throw new Error(`scheduled task ${id} does not exist`)
    const author = taskAuthors.get(id)
    if (!author) throw new Error(`scheduled task ${id} was not created by these tests`)
    return {
      id: Number(row.id),
      oid: author.oid,
      enabled: row.enabled,
      nextRunAt: row.next_run_at,
      lastEnqueuedAt: row.last_enqueued_at,
      lastRunAt: row.last_run_at,
      lastRunId: row.last_run_id,
      lastError: row.last_error
    }
  },
  /** Runs the task now, as its author pressing "Run now" in Console. */
  async runNow(task: ScheduledTask): Promise<ScheduledTaskRun> {
    const session = await authorSession(task)
    const { status, body } = await session.request('POST', `/api/v1/console/scheduled_tasks/${task.oid}/run`)
    if (status === 202) return { started: true, runId: body.data.run_id }
    return { started: false, status, message: body?.error?.message }
  },
  /** The author turns the task off in Console. */
  async disable(task: ScheduledTask): Promise<void> {
    const session = await authorSession(task)
    await session.ok('PATCH', `/api/v1/console/scheduled_tasks/${task.oid}`, { data: { enabled: false } })
  },
  /** The bot leaves the channel, as Console's channel catalog would record it. */
  async botLeaves(channel: Channel): Promise<void> {
    await ironControlDb`
      update slack_bot_channels set active = false, updated_at = now() at time zone 'UTC'
      where channel_id = ${channel.id}`
  }
}

/** The password e2e gives scheduled-task authors, who otherwise sign in with Slack. */
const AUTHOR_PASSWORD = 'e2e-scheduled-task-author'
/** Each task these tests created: its Console id and its author. */
const taskAuthors = new Map<number, { oid: string; email: string }>()

async function authorSession(task: ScheduledTask) {
  const author = taskAuthors.get(task.id)
  if (!author) throw new Error(`scheduled task ${task.id} was not created by these tests`)
  return consoleSession(author.email, AUTHOR_PASSWORD)
}

/**
 * A Console browser session: signs in through the password form, then calls
 * the Console API with the session cookie and the Console origin, as the SPA
 * does.
 */
async function consoleSession(email: string, password: string) {
  const login = await fetch(`${consoleUrl}/login`, {
    method: 'POST',
    redirect: 'manual',
    headers: { origin: consoleUrl, 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ email, password })
  })
  const cookie = login.headers
    .getSetCookie()
    .map(header => header.split(';')[0])
    .join('; ')
  if (login.status !== 302 && login.status !== 303) throw new Error(`Console sign-in failed: ${login.status}`)
  if (!cookie) throw new Error('Console sign-in set no session cookie')
  const session = {
    async request(method: string, path: string, body?: unknown): Promise<{ status: number; body: any }> {
      const response = await fetch(`${consoleUrl}${path}`, {
        method,
        headers: { cookie, origin: consoleUrl, 'content-type': 'application/json' },
        ...(body === undefined ? {} : { body: JSON.stringify(body) })
      })
      const text = await response.text()
      let payload: unknown = text
      try {
        payload = text ? JSON.parse(text) : undefined
      } catch {
        // Non-JSON bodies stay text.
      }
      return { status: response.status, body: payload }
    },
    async ok(method: string, path: string, body?: unknown): Promise<any> {
      const { status, body: payload } = await session.request(method, path, body)
      if (status < 200 || status > 299) throw new Error(`${method} ${path} failed: ${status} ${JSON.stringify(payload)}`)
      return payload
    }
  }
  return session
}

/** An API client an operator created in the console, with its one dedicated principal. */
export type ApiClient = { clientId: string; secret: string; principalId: string; principalForeignId: string }

/** A session API event, as its SSE frame named and carried it. */
export type SessionEvent = { event: string; data: string }

/** Whether an event ends its execution. */
function isTerminalEvent(event: SessionEvent | undefined): boolean {
  return event !== undefined && ['session.execution_completed', 'session.execution_failed', 'session.execution_cancelled'].includes(event.event)
}

/** The execution a control event names. Output lines are raw harness lines and name none. */
function eventExecutionId(event: SessionEvent): string | undefined {
  if (event.event === 'session.output.line') return undefined
  try {
    const payload = JSON.parse(event.data)
    return typeof payload?.execution_id === 'string' ? payload.execution_id : undefined
  } catch {
    return undefined
  }
}

/**
 * Reads a session's SSE event stream, returning the frames collected once
 * `stop` sees enough of them or the stream ends.
 */
async function readSessionEvents(
  threadKey: string,
  query: Record<string, string>,
  authorization: string,
  stop: (events: SessionEvent[]) => boolean
): Promise<SessionEvent[]> {
  const params = new URLSearchParams(query)
  const response = await fetch(`${apiUrl}/api/session/${encodeURIComponent(threadKey)}/events?${params}`, {
    headers: { authorization: `Bearer ${authorization}` },
    signal: AbortSignal.timeout(turnTimeoutMs)
  })
  if (!response.ok || !response.body) throw new Error(`event stream failed: ${response.status}`)
  const events: SessionEvent[] = []
  let buffer = ''
  const decoder = new TextDecoder()
  const reader = response.body.getReader()
  for (let chunk = await reader.read(); !chunk.done; chunk = await reader.read()) {
    buffer += decoder.decode(chunk.value, { stream: true })
    let end: number
    while ((end = buffer.indexOf('\n\n')) >= 0) {
      const frame = buffer.slice(0, end).split('\n')
      buffer = buffer.slice(end + 2)
      const event = frame.find(line => line.startsWith('event:'))?.slice(6).trim()
      if (!event) continue
      const data = frame.filter(line => line.startsWith('data:')).map(line => line.slice(5).trimStart()).join('\n')
      events.push({ event, data })
      if (stop(events)) {
        await reader.cancel()
        return events
      }
    }
  }
  return events
}

/**
 * Machine callers of the session API: created as an operator would in the
 * console, then calling api-rs with the access token they exchange their
 * secret for.
 */
export const apiClients = {
  async create(options: { scopes?: string[] } = {}): Promise<ApiClient> {
    const name = `e2e-${randomUUID().slice(0, 8)}`
    // The client's dedicated principal, with the default sandbox capabilities.
    const { data: principal } = await ironControl.api.ok('POST', '/api/v1/principals', {
      data: { kind: 'unknown', foreign_id: name, name }
    })
    const admin = await consoleSession(consoleAdminEmail, consoleAdminPassword)
    const { data: client } = await admin.ok('POST', '/api/v1/console/api_clients', {
      data: { name, scopes: options.scopes ?? ['sessions:read', 'sessions:write'], principal: principal.id }
    })
    return { clientId: client.id, secret: client.secret, principalId: principal.id, principalForeignId: name }
  },
  /** Rotates the client's secret; returns the client with its new secret. */
  async rotate(client: ApiClient): Promise<ApiClient> {
    const admin = await consoleSession(consoleAdminEmail, consoleAdminPassword)
    const { data } = await admin.ok('POST', `/api/v1/console/api_clients/${client.clientId}/rotate_secret`)
    return { ...client, secret: data.secret }
  },
  async disable(client: ApiClient): Promise<void> {
    const admin = await consoleSession(consoleAdminEmail, consoleAdminPassword)
    await admin.ok('POST', `/api/v1/console/api_clients/${client.clientId}/disable`)
  },
  /** The client credentials token exchange, authenticating with HTTP Basic. */
  async token(client: ApiClient, scope?: string): Promise<{ status: number; body: any }> {
    const response = await fetch(`${apiUrl}/oauth/token`, {
      method: 'POST',
      headers: {
        authorization: `Basic ${btoa(`${client.clientId}:${client.secret}`)}`,
        'content-type': 'application/x-www-form-urlencoded'
      },
      body: new URLSearchParams({ grant_type: 'client_credentials', ...(scope ? { scope } : {}) })
    })
    return { status: response.status, body: await response.json() }
  },
  /** api-rs, called with an API client's access token. */
  as(accessToken: string) {
    return {
      ...operatorApi(apiUrl, accessToken),
      /** A session's events for one execution, through its terminal event. */
      async events(threadKey: string, executionId: string): Promise<SessionEvent[]> {
        const events = await readSessionEvents(
          threadKey,
          { execution_id: executionId, after_event_id: '0' },
          accessToken,
          events => isTerminalEvent(events.at(-1))
        )
        if (!events.some(isTerminalEvent)) {
          throw new Error(`event stream ended before execution ${executionId} finished: ${JSON.stringify(events)}`)
        }
        return events
      }
    }
  }
}

/**
 * A conversation with the bot: a channel thread, or a DM, which slackbotv2
 * runs as one conversation-wide session.
 */
export class Thread {
  /** The user message that triggered each turn, and that turn's script token. */
  private readonly triggers: string[]
  private readonly scripts: Array<string | undefined>
  private readonly dm: boolean
  private turnsSeen = 0
  /** Brain executions an earlier nextBrainTurn already reported, so replay skips them. */
  private readonly brainTurns = new Set<string>()

  constructor(
    readonly channel: Channel,
    readonly ts: string,
    script?: Script,
    options: { dm?: boolean; trigger?: string } = {}
  ) {
    this.triggers = [options.trigger ?? ts]
    this.scripts = [script?.token]
    this.dm = options.dm ?? false
  }

  get key(): string {
    return this.dm ? `slack:${this.channel.id}:` : `slack:${this.channel.id}:${this.ts}`
  }

  /** Follows up with a message that mentions the bot (in a DM, any message), by USER unless `as` says otherwise. */
  async mention(text: string, script?: Script, options: { as?: SlackUser } = {}): Promise<void> {
    this.triggers.push(await this.send(text, script, options.as))
    this.scripts.push(script?.token)
  }

  /**
   * Mentions the bot while a turn is running. The running turn takes the
   * message (as steering, or a stop), so it starts no turn of its own.
   */
  async interject(text: string, script?: Script, options: { as?: SlackUser } = {}): Promise<string> {
    return this.send(text, script, options.as)
  }

  /** Posts a reply that does not mention the bot. */
  async post(text: string, options: { as?: SlackUser } = {}): Promise<string> {
    return slack.post(text, { channel: this.channel, as: options.as, threadTs: this.dm ? undefined : this.ts })
  }

  /** Waits until the latest turn's model request is in flight, held by its script's delay. */
  async inFlight(): Promise<void> {
    const token = this.scripts.at(-1)
    if (!token) throw new Error(`the latest turn of ${this.key} has no script to wait on`)
    await eventually(`the model request of ${this.key}'s latest turn`, turnTimeoutMs, async () =>
      (await modelRequests(token)).length > 0 ? true : undefined
    )
  }

  /** The assistant status Slack shows on the thread now; empty when none is shown. */
  async status(): Promise<string> {
    const query = new URLSearchParams({ channel: this.channel.id, thread_ts: this.ts })
    const { status } = (await (await fetch(`${slackUrl}/_e2e/status?${query}`)).json()) as { status: string }
    return status
  }

  private async send(text: string, script: Script | undefined, as: SlackUser = USER): Promise<string> {
    return postMention(this.channel, as, text, script, this.dm ? undefined : this.ts, { dm: this.dm })
  }

  /**
   * Waits for the next turn: its execution reaches a terminal state and its
   * Slack reply finishes streaming. A turn must produce exactly one reply.
   */
  async nextTurn(): Promise<Turn> {
    const index = this.turnsSeen++
    const trigger = this.triggers[index]
    if (!trigger) throw new Error(`turn ${index + 1} was never triggered in ${this.key}`)

    const execution = await eventually(`execution ${index + 1} of ${this.key}`, turnTimeoutMs, async () => {
      const executions = await this.executions()
      const execution = executions[index]
      return execution && isTerminal(execution.status) ? execution : undefined
    })
    // A failure names the execution and the replies so far, so it can be read without the stack.
    const seen = `execution ${JSON.stringify(execution)}`
    let lastReplies: SlackMessage[] = []
    const replies = await eventually(`reply to turn ${index + 1} of ${this.key}`, renderTimeoutMs, async () => {
      lastReplies = await this.repliesTo(index)
      return lastReplies.length > 0 && lastReplies.every(reply => !reply.streaming) ? lastReplies : undefined
    }).catch(error => {
      throw new Error(`${error.message}; ${seen}; replies ${JSON.stringify(lastReplies.map(({ ts, text, streaming }) => ({ ts, text, streaming })))}`)
    })
    if (replies.length !== 1) {
      throw new Error(`turn ${index + 1} of ${this.key} produced ${replies.length} replies: ${JSON.stringify(replies)}`)
    }
    const [sandbox]: Sandbox[] = await db`
      select payload->>'sandbox_id' as id, payload->>'harness_type' as harness,
        payload->>'sandbox_ready_source' as source
      from session_events
      where execution_id = ${execution.execution_id} and event_type = 'session.sandbox_ready'
      order by event_id desc limit 1`
    if (!sandbox) throw new Error(`turn ${index + 1} of ${this.key} recorded no ready sandbox; ${seen}`)
    const token = this.scripts[index]
    return {
      reply: replies[0]!.text ?? '',
      context: (replies[0]!.blocks ?? [])
        .filter(block => block.type === 'context')
        .flatMap(block => block.elements ?? [])
        .map(element => element.text ?? '')
        .join(' '),
      execution,
      sandbox,
      request: token ? (await modelRequests(token))[0] : undefined
    }
  }

  /**
   * Waits for the next turn of a `nanocode` thread. Those turns run in
   * centaur-brain and leave no `session_executions` or `session_events` rows,
   * so the turn's completion and first-token latency come from the session
   * event stream api-rs proxies from the brain, and its reply from Slack as
   * nextTurn reads it. A turn must produce exactly one reply.
   */
  async nextBrainTurn(): Promise<BrainTurn> {
    const index = this.turnsSeen++
    const trigger = this.triggers[index]
    if (!trigger) throw new Error(`turn ${index + 1} was never triggered in ${this.key}`)

    // The ingress creates the session after it receives the mention; the
    // event stream of a thread with no session is a 404.
    await eventually(`session of ${this.key}`, turnTimeoutMs, async () => {
      const [session] = await db`select 1 as ok from sessions where thread_key = ${this.key}`
      return session
    })
    // The stream is session-scoped because the turn's request id is chosen by
    // the brain: replay the log and stop at the first terminal event of an
    // execution no earlier call reported.
    let events: SessionEvent[]
    try {
      events = await readSessionEvents(this.key, { after_event_id: '0' }, apiKey, events => {
        const executionId = eventExecutionId(events.at(-1)!)
        return isTerminalEvent(events.at(-1)) && executionId !== undefined && !this.brainTurns.has(executionId)
      })
    } catch (error) {
      throw new Error(`the event stream of ${this.key} failed while waiting for turn ${index + 1}: ${error}`)
    }
    const terminal = events.at(-1)!
    const executionId = eventExecutionId(terminal)
    if (!isTerminalEvent(terminal) || executionId === undefined) {
      throw new Error(`turn ${index + 1} of ${this.key} streamed no terminal event: ${JSON.stringify(terminal)}`)
    }
    this.brainTurns.add(executionId)
    const turnEvents = events.filter(event => eventExecutionId(event) === executionId)

    const payload = JSON.parse(terminal.data)
    const status = terminal.event === 'session.execution_completed'
      ? 'completed'
      : terminal.event === 'session.execution_cancelled' ? 'cancelled' : 'failed'
    const execution: Execution = {
      execution_id: executionId,
      status,
      error: typeof payload?.error === 'string' ? payload.error : null
    }
    const firstToken = turnEvents.find(event => event.event === 'session.first_token')
    const latencyMs = firstToken ? JSON.parse(firstToken.data)?.latency_ms : undefined
    if (typeof latencyMs !== 'number') {
      throw new Error(`turn ${index + 1} of ${this.key} recorded no first token: ${JSON.stringify(turnEvents)}`)
    }

    // A failure names the execution and the replies so far, so it can be read without the stack.
    const seen = `execution ${JSON.stringify(execution)}`
    let lastReplies: SlackMessage[] = []
    const replies = await eventually(`reply to turn ${index + 1} of ${this.key}`, renderTimeoutMs, async () => {
      lastReplies = await this.repliesTo(index)
      return lastReplies.length > 0 && lastReplies.every(reply => !reply.streaming) ? lastReplies : undefined
    }).catch(error => {
      throw new Error(`${error.message}; ${seen}; replies ${JSON.stringify(lastReplies.map(({ ts, text, streaming }) => ({ ts, text, streaming })))}`)
    })
    if (replies.length !== 1) {
      throw new Error(`turn ${index + 1} of ${this.key} produced ${replies.length} replies: ${JSON.stringify(replies)}`)
    }
    return { reply: replies[0]!.text ?? '', execution, firstTokenMs: latencyMs }
  }

  async nextCancelledExecution(): Promise<Execution> {
    const index = this.turnsSeen++
    return eventually(`cancelled execution ${index + 1} of ${this.key}`, turnTimeoutMs, async () => {
      const execution = (await this.executions())[index]
      if (!execution || !isTerminal(execution.status)) return undefined
      if (execution.status !== 'cancelled') throw new Error(`expected cancellation, got ${JSON.stringify(execution)}`)
      return execution
    })
  }

  /** Every bot message in the conversation so far, oldest first. */
  async botReplies(): Promise<SlackMessage[]> {
    return this.repliesTo(0, { untilNextTrigger: false })
  }

  /** Sandboxes that served this thread, in the order they became ready. */
  async sandboxes(): Promise<Array<{ id: string; harness: string }>> {
    return db`
      select id, harness from (
        select distinct on (payload->>'sandbox_id') payload->>'sandbox_id' as id,
          payload->>'harness_type' as harness, event_id
        from session_events
        where thread_key = ${this.key} and event_type = 'session.sandbox_ready'
        order by payload->>'sandbox_id', event_id
      ) ready order by event_id`
  }

  private async executions(): Promise<Execution[]> {
    return db`
      select execution_id, status, error from session_executions
      where thread_key = ${this.key} order by created_at`
  }

  /** Bot messages posted after a turn's trigger and before the next turn's trigger. */
  private async repliesTo(index: number, options: { untilNextTrigger?: boolean } = {}): Promise<SlackMessage[]> {
    const { messages } = this.dm
      ? await slackApi<{ messages: SlackMessage[] }>(USER, 'conversations.history', { channel: this.channel.id })
      : await slackApi<{ messages: SlackMessage[] }>(USER, 'conversations.replies', {
        channel: this.channel.id,
        ts: this.ts
      })
    messages.sort((a, b) => (a.ts < b.ts ? -1 : 1))
    const next = options.untilNextTrigger === false ? undefined : this.triggers[index + 1]
    return messages.filter(message =>
      message.bot_id === BOT.id && message.ts > this.triggers[index]! && (!next || message.ts < next)
    )
  }
}

async function postMention(
  channel: Channel,
  user: SlackUser,
  text: string,
  script: Script | undefined,
  threadTs?: string,
  options: { dm?: boolean } = {}
): Promise<string> {
  if (script) await registerScript(script)
  const posted = await slackApi<{ ts: string }>(user, 'chat.postMessage', {
    channel: channel.id,
    text: `${options.dm ? '' : `<@${BOT.userId}> `}${text}${script ? ` ${script.token}` : ''}`,
    ...(threadTs ? { thread_ts: threadTs } : {})
  })
  return posted.ts
}

async function registerScript(script: Script): Promise<void> {
  const response = await fetch(`${modelUrl}/_e2e/replies`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      match: script.token, text: script.text, delayMs: script.delayMs, error: script.error, run: script.run,
      cell: script.cell
    })
  })
  if (!response.ok) throw new Error(`registering the model script failed: ${response.status}`)
}

async function modelRequests(token: string): Promise<ModelRequest[]> {
  const response = await fetch(`${modelUrl}/_e2e/requests?match=${encodeURIComponent(token)}`)
  const recorded = (await response.json()) as Array<
    Pick<ModelRequest, 'provider' | 'model' | 'credential' | 'account' | 'body'> & { conversation: unknown[] }
  >
  return recorded.map(({ conversation, ...request }) => {
    const body = request.body
    const history = conversation as Array<{ role?: string; content?: unknown }>
    return {
      ...request,
      effort: body.reasoning?.effort ?? body.output_config?.effort,
      userText: lastBlockText(history.filter(item => item.role === 'user').at(-1)?.content),
      userMessage: contentText(history.filter(item => item.role === 'user').at(-1)?.content),
      assistantTurns: history
        .filter(item => item.role === 'assistant')
        .map(item => contentText(item.content)),
      // OpenAI carries system text in `instructions`, Anthropic in `system`.
      prompt: [body.instructions, body.system, ...history.map(item => item.content)]
        .map(contentText)
        .join('\n')
    }
  })
}

/** Codex's code mode returns each nested exec_command result as JSON; keep only what the command printed. */
function commandText(output: string): string {
  return output
    .split('\n')
    .map(line => {
      try {
        const parsed = JSON.parse(line)
        return typeof parsed?.output === 'string' ? parsed.output : line
      } catch {
        return line
      }
    })
    .join('\n')
}

function lastBlockText(content: unknown): string {
  return Array.isArray(content) ? contentText(content.slice(-1)) : contentText(content)
}

function contentText(content: unknown): string {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) return ''
  return content.map(part => (typeof part?.text === 'string' ? part.text : '')).join('\n')
}

function isTerminal(status: string): boolean {
  return ['completed', 'failed', 'cancelled'].includes(status)
}

/** Polls until `check` returns a value, or fails naming what it waited for. */
export async function eventually<T>(what: string, timeoutMs: number, check: () => Promise<T | undefined>): Promise<T> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const value = await check()
    if (value !== undefined) return value
    await Bun.sleep(1_000)
  }
  throw new Error(`timed out after ${timeoutMs / 1000}s waiting for ${what}`)
}

async function slackApi<T>(user: SlackUser, method: string, body: Record<string, string>): Promise<T> {
  const response = await fetch(`${slackUrl}/api/${method}`, {
    method: 'POST',
    headers: { authorization: `Bearer ${user.token}`, 'content-type': 'application/json' },
    body: JSON.stringify(body)
  })
  const payload = await response.json() as { ok: boolean; error?: string } & T
  if (!payload.ok) throw new Error(`${method} failed: ${payload.error}`)
  return payload
}

async function kubectl(options: { stdin?: string; namespace?: string }, ...args: string[]): Promise<string> {
  const proc = Bun.spawn(['kubectl', '-n', options.namespace ?? namespace, ...args], {
    stdin: options.stdin === undefined ? 'ignore' : new Blob([options.stdin]),
    stdout: 'pipe',
    stderr: 'pipe'
  })
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited
  ])
  if (code !== 0) throw new Error(`kubectl ${args.join(' ')} failed: ${stderr.trim()}`)
  return stdout.trim()
}

async function docker(stdin: string | undefined, ...args: string[]): Promise<string> {
  const proc = Bun.spawn(['docker', ...args], {
    stdin: stdin === undefined ? 'ignore' : new Blob([stdin]),
    stdout: 'pipe',
    stderr: 'pipe'
  })
  const [stdout, stderr, code] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited
  ])
  if (code !== 0) throw new Error(`docker ${args.join(' ')} failed: ${stderr.trim()}`)
  return stdout.trim()
}

function required(name: string): string {
  const value = process.env[name]
  if (!value) throw new Error(`${name} is required; run the tests with e2e/stack.sh test`)
  return value
}
