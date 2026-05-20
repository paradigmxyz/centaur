'use client'

import type { CSSProperties } from 'react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'

type Reply = {
  from: 'bot' | 'user'
  text: string
  time: string
  card?: TaskCardData
  who?: string
  glyph?: string
  color?: string
}

type TaskCardData = {
  title: string
  items: Array<{
    command: string
    output: string
    exitCode?: number
  }>
}

type ThreadData = {
  id: string
  channel: string
  // Short, human-authored title that displays in the threads sidebar — does
  // NOT come from the message body. Keeps the list readable when bodies are
  // long, mention-heavy, or contain markdown-y noise.
  title: string
  parent: {
    who: string
    glyph: string
    color: string
    time: string
    body: string
  }
  replies: Reply[]
}

const threadData: ThreadData[] = [
  {
    id: 'tempo-merch',
    channel: 'builds',
    title: 'Build a Tempo merch store',
    parent: {
      who: 'Perry Dime',
      glyph: 'P',
      color: '#28c26a',
      time: 'Today at 9:02 AM',
      body: ' build a merch store: stablecoin checkout, NFT receipt, one-time redemption for shipping details, and an admin view for fulfillment. Use Vite, Cloudflare Workers, D1, R2, Tempo Accounts, wagmi, viem, and MPP image generation.',
    },
    replies: [
      {
        from: 'bot',
        text: 'I scaffolded the app, wired Tempo Accounts from the reference repo, added the redemption data model, and deployed a first preview. The flow now has product pages, stablecoin checkout, generated artwork, NFT purchase, and a seller dashboard.',
        time: '9:18 AM',
        card: {
          title: 'Building preview...',
          items: [
            {
              command: "gh repo create tempoxyz/merch --private --source . --push",
              output:
                '{"repository":"tempoxyz/merch","visibility":"private","default_branch":"main"}',
            },
            {
              command: "rg \"createConfig|tempo\" ../tip.bot src",
              output:
                'src/lib/accounts.ts:import { tempo } from "viem/chains"\nsrc/lib/accounts.ts:export const wagmiConfig = createConfig({...})',
            },
            {
              command: "npm run build && npx wrangler deploy",
              output:
                'vite built in 2.8s\nUploaded merch-preview worker\nhttps://merch.tempo-dev.workers.dev',
            },
          ],
        },
      },
      {
        from: 'user',
        text: 'Update wagmi and accounts to the latest compatible versions.',
        time: '9:24 AM',
        who: 'Derek Cofausper',
        glyph: 'D',
        color: '#0f766e',
      },
      {
        from: 'bot',
        text: 'Opened a dependency PR that moves wagmi, viem, Accounts, and mppx forward together. I used package-manager overrides for the prerelease wagmi packages so CI resolves the same build locally and on GitHub Actions.',
        time: '9:26 AM',
        card: {
          title: 'Updating dependencies...',
          items: [
            {
              command: "pnpm up wagmi viem @tempo-xyz/accounts mppx",
              output:
                '@tempo-xyz/accounts 0.10.3 -> 0.10.6\nwagmi 3.6.12 -> pkg.pr.new/wagmi@838a05e\nviem already current\nmppx already current',
            },
            {
              command: "pnpm install --lockfile-only && pnpm install --frozen-lockfile",
              output: 'Lockfile is up to date\nDone in 4.1s',
            },
          ],
        },
      },
      {
        from: 'user',
        text: 'Checkout is throwing: TypeError: Do not know how to serialize a BigInt.',
        time: '9:32 AM',
        who: 'Derek Cofausper',
        glyph: 'D',
        color: '#0f766e',
      },
      {
        from: 'bot',
        text: 'Fixed the purchase payload by serializing BigInt values before JSON.stringify and sending product.price as a string. That matches the /api/purchases handler, which stores the value in D1 without numeric coercion.',
        time: '9:34 AM',
        card: {
          title: 'Fixing purchase JSON...',
          items: [
            {
              command: "rg \"JSON.stringify|price\" src/routes src/api",
              output:
                'src/routes/shop.$productId.tsx: JSON.stringify(body, bigintReplacer)\nsrc/api/purchases.ts: price: string',
            },
            {
              command: "npm run typecheck && npm test",
              output: 'typecheck passed\npurchase payload tests passed',
            },
          ],
        },
      },
      {
        from: 'user',
        text: 'The wallet says eth_signRawTransaction does not exist. Is that the app or the relay?',
        time: '10:05 AM',
        who: 'Perry Dime',
        glyph: 'P',
        color: '#28c26a',
      },
      {
        from: 'bot',
        text: 'It is relay configuration. The checkout batches an ERC20 transfer and NFT purchase, so the wallet asks the relay to sponsor and sign the raw transaction. I opened one PR to set the fee-payer secret during deploy and another in Accounts to return a clear fee-payer error instead of a generic method-not-found response.',
        time: '10:09 AM',
        card: {
          title: 'Tracing relay behavior...',
          items: [
            {
              command: "rg \"feePayer|eth_signRawTransaction|Handler.relay\" src packages",
              output:
                'worker/api/relay.ts: Handler.relay({ feePayer, transports })\npackages/accounts/src/relay.ts: eth_signRawTransaction requires configured sponsorship',
            },
            {
              command: "pnpm test relay",
              output:
                'passes eth_sendRawTransaction through\nreturns a fee-payer setup error for eth_signRawTransaction when sponsorship is missing',
            },
          ],
        },
      },
      {
        from: 'user',
        text: 'Now Review Transaction reports: too many connections from this IP.',
        time: '11:12 AM',
        who: 'Perry Dime',
        glyph: 'P',
        color: '#28c26a',
      },
      {
        from: 'bot',
        text: 'That response is coming from the upstream Tempo RPC, not the merch app. The Worker is proxying eth_fillTransaction, and the RPC is rate-limiting the shared Cloudflare egress IP. The clean fix is to configure the relay with a dedicated RPC transport for Tempo mainnet.',
        time: '11:15 AM',
        card: {
          title: 'Checking RPC path...',
          items: [
            {
              command: "call logs query '{\"service\":\"merch-worker\",\"contains\":\"too many connections\"}'",
              output:
                '{"method":"eth_fillTransaction","upstream":"https://rpc.tempo.xyz","error":"too many connections from this IP"}',
            },
            {
              command: "rg \"transports\" node_modules/@tempo-xyz/accounts src",
              output:
                'Handler.relay({ transports: { [tempo.id]: http(env.TEMPO_RPC_URL) } })',
            },
          ],
        },
      },
      {
        from: 'user',
        text: 'Add the custom RPC as an env var and use it in the relay.',
        time: '11:18 AM',
        who: 'Derek Cofausper',
        glyph: 'D',
        color: '#0f766e',
      },
      {
        from: 'bot',
        text: 'Added TEMPO_RPC_URL to the Worker environment and wired the relay transport to use it for Tempo mainnet fill and sponsorship calls.',
        time: '11:22 AM',
        card: {
          title: 'Wiring private RPC...',
          items: [
            {
              command: "npx wrangler secret put TEMPO_RPC_URL",
              output: 'Secret TEMPO_RPC_URL uploaded to merch-preview',
            },
            {
              command: "npm run typecheck && npm run build",
              output: 'typecheck passed\nvite built in 2.6s',
            },
          ],
        },
      },
      {
        from: 'user',
        text: 'Secret added. Deploy and dogfood the checkout flow.',
        time: '11:25 AM',
        who: 'Perry Dime',
        glyph: 'P',
        color: '#28c26a',
      },
      {
        from: 'bot',
        text: 'No issues found. I deployed the Worker, opened the preview, completed account connection, generated artwork, ran checkout, confirmed the NFT purchase, and verified the redemption data appears in the admin view. Deployment: https://merch.tempo-dev.workers.dev',
        time: '11:31 AM',
        card: {
          title: 'Deploying and dogfooding...',
          items: [
            {
              command: "npx wrangler deploy",
              output: 'Uploaded merch-preview worker\nhttps://merch.tempo-dev.workers.dev',
            },
            {
              command: "npm run dogfood -- --url https://merch.tempo-dev.workers.dev",
              output:
                'account connect passed\nartwork generation passed\ncheckout passed\nredemption form passed\nadmin fulfillment view passed\n0 issues found',
            },
          ],
        },
      },
    ],
  },
  {
    id: 'prod-alert',
    channel: 'prod-alerts',
    title: 'Investigate elevated API 5xx alert',
    parent: {
      who: 'Perry Dime',
      glyph: 'P',
      color: '#0f766e',
      time: 'Today at 10:32 AM',
      body: ' can you investigate the elevated API error alert? Please check observability first, then Kubernetes if it looks real.',
    },
    replies: [
      {
        from: 'bot',
        text: 'I pulled the alert window from observability and the spike is real: API 5xx increased over the last ten minutes, concentrated on agent execution requests. Latency also widened, but only on the API service.',
        time: '10:33 AM',
        card: {
          title: 'Calling tools...',
          items: [
            {
              command:
                "/bin/bash -lc \"call observability query '{\\\"service\\\":\\\"api\\\",\\\"metric\\\":\\\"http_5xx_rate\\\",\\\"window\\\":\\\"10m\\\"}'\"",
              output:
                '{"status":"success","data":{"resultType":"vector","result":[{"metric":{"service":"api","route":"/agent/execute","status_class":"5xx"},"value":[1779028382.441,"0.0431"]}]},"stats":{"seriesFetched":18,"executionTimeMsec":37}}',
            },
            {
              command:
                "/bin/bash -lc \"call observability logs '{\\\"query\\\":\\\"service=api level=error route=/agent/execute\\\",\\\"limit\\\":20}'\"",
              output:
                '{"hits":[{"_time":"2026-05-17T08:33:14.928Z","_stream":"{service=\\"api\\",pod=\\"centaur-api-7d9f6\\"}","level":"error","event":"sandbox_attach_timeout","execution_id":"exe_01HX7R6V","thread_key":"slack:C0A87C21805:1779028272.441829"},{"_time":"2026-05-17T08:33:18.102Z","level":"error","event":"execution_failed","route":"/agent/execute"}],"limit":20}',
            },
            {
              command:
                "/bin/bash -lc \"call observability query '{\\\"service\\\":\\\"api\\\",\\\"metric\\\":\\\"p95_latency\\\",\\\"group_by\\\":\\\"route\\\",\\\"window\\\":\\\"10m\\\"}'\"",
              output:
                '{"status":"success","data":{"resultType":"vector","result":[{"metric":{"route":"/agent/execute"},"value":[1779028411.006,"4.812"]},{"metric":{"route":"/health"},"value":[1779028411.006,"0.018"]}]}}',
            },
          ],
        },
      },
      {
        from: 'user',
        text: 'Can you see which pods are involved?',
        time: '10:34 AM',
      },
      {
        from: 'bot',
        text: 'Using the Kubernetes tool, two API pods are healthy and one restarted twice. Its logs show repeated sandbox attach timeouts. The worker pods are healthy, and Postgres has no connection saturation.',
        time: '10:35 AM',
        card: {
          title: 'Calling tools...',
          items: [
            {
              command:
                "/bin/bash -lc \"call kubernetes get_pods '{\\\"namespace\\\":\\\"centaur\\\",\\\"selector\\\":\\\"app.kubernetes.io/name=centaur\\\"}'\"",
              output:
                '{"apiVersion":"v1","items":[{"metadata":{"name":"centaur-api-7d9f6","namespace":"centaur","uid":"0f89c6e2"},"status":{"phase":"Running","containerStatuses":[{"name":"api","ready":true,"restartCount":2}]}},{"metadata":{"name":"centaur-api-84cc9","namespace":"centaur","uid":"be722a1d"},"status":{"phase":"Running","containerStatuses":[{"name":"api","ready":true,"restartCount":0}]}}]}',
            },
            {
              command:
                "/bin/bash -lc \"call kubernetes logs '{\\\"namespace\\\":\\\"centaur\\\",\\\"pod\\\":\\\"centaur-api-7d9f6\\\",\\\"tail\\\":50}'\"",
              output:
                '{"pod":"centaur-api-7d9f6","container":"api","lines":["{\\"level\\":\\"error\\",\\"event\\":\\"sandbox_attach_timeout\\",\\"execution_id\\":\\"exe_01HX7R6V\\",\\"thread_key\\":\\"slack:C0A87C21805:1779028272.441829\\",\\"elapsed_ms\\":30001}"],"truncated":false}',
            },
          ],
        },
      },
      {
        from: 'user',
        text: 'What should we do first?',
        time: '10:36 AM',
      },
      {
        from: 'bot',
        text: 'First step is to drain the restarted API pod and watch the observability error rate for five minutes. If errors drop, follow up by inspecting its sandbox attach logs and recent Kubernetes events before rolling anything.',
        time: '10:36 AM',
      },
    ],
  },
  {
    id: 'warehouse-tool',
    channel: 'tools',
    title: 'Add a warehouse-lookup tool',
    parent: {
      who: 'Alex Kim',
      glyph: 'A',
      color: '#7c3aed',
      time: 'Today at 10:48 AM',
      body: ' can you sketch the fastest path to add a warehouse lookup tool with a 1Password-backed API key?',
    },
    replies: [
      {
        from: 'bot',
        text: 'Create a tool directory with client.py and pyproject.toml, declare the secret reference, call secret("WAREHOUSE_API_KEY") from the client, and let iron-proxy replace the placeholder at the outbound request boundary.',
        time: '10:49 AM',
        card: {
          title: 'Calling tools...',
          items: [
            {
              command: "/bin/bash -lc 'call discover tools'",
              output:
                '{"tool":"tools","description":"Tool authoring helpers","methods":[{"name":"create_tool","parameters":{"name":{"type":"str","required":true}}},{"name":"inspect_tool_schema","parameters":{"path":{"type":"str","required":true}}},{"name":"validate_tool","parameters":{"path":{"type":"str","required":true}}}]}',
            },
            {
              command:
                "/bin/bash -lc \"call secrets resolve '{\\\"name\\\":\\\"WAREHOUSE_API_KEY\\\",\\\"source\\\":\\\"1password\\\"}'\"",
              output:
                '{"name":"WAREHOUSE_API_KEY","provider":"1password","resolved":true,"secret_ref":"op://prod/warehouse/api-token/credential","placeholder":"WAREHOUSE_API_KEY","value":"[redacted]","version":"4"}',
            },
            {
              command:
                "/bin/bash -lc \"call filesystem list '{\\\"path\\\":\\\"overlay/tools\\\"}'\"",
              output:
                '{"path":"overlay/tools","entries":[{"name":"warehouse","type":"directory"},{"name":"observability","type":"directory"},{"name":"kubernetes","type":"directory"}],"errno":0}',
            },
          ],
        },
      },
      {
        from: 'user',
        text: 'Should this live in the base repo or an overlay?',
        time: '10:50 AM',
      },
      {
        from: 'bot',
        text: 'Put organization-specific warehouse logic in an overlay. The base repo should keep generic integrations only; the overlay can own the tool, workflow wrapper, persona prompt, and deployment-specific secret mapping.',
        time: '10:51 AM',
      },
      {
        from: 'user',
        text: 'Cool, add the checklist to the overlay guide.',
        time: '10:52 AM',
      },
      {
        from: 'bot',
        text: 'Done. I split the checklist into implementation, secret wiring, local verification, and deploy verification so each step maps to an observable state.',
        time: '10:53 AM',
      },
    ],
  },
]

function BotAvatar({ accent }: { glyph?: string; accent: string }) {
  // Centaur bot avatar — uses the rounded-square Slack icon mark instead of
  // a colored letter so it reads as the real product mark inside the demo.
  return (
    <div
      className="thread-panel-avatar thread-panel-avatar-bot"
      style={{ '--thread-accent': accent } as CSSProperties}
    >
      <img src="/brand/slack-icon.svg" alt="Centaur" />
    </div>
  )
}

function TypingDots() {
  return (
    <span className="thread-panel-typing" aria-label="Centaur is typing">
      <span />
      <span />
      <span />
    </span>
  )
}

function TaskCard({
  accent,
  card,
  running,
}: {
  accent: string
  card: TaskCardData
  running: boolean
}) {
  const [isExpanded, setIsExpanded] = useState(false)
  const [visibleCount, setVisibleCount] = useState(running ? 1 : card.items.length)

  useEffect(() => {
    if (!running) {
      setVisibleCount(card.items.length)
      return
    }

    setVisibleCount((count) => Math.min(card.items.length, Math.max(1, count)))
    const timer = window.setInterval(() => {
      setVisibleCount((count) => Math.min(card.items.length, count + 1))
    }, 540)

    return () => window.clearInterval(timer)
  }, [card.items.length, running])

  return (
    <div className="thread-task-card" style={{ '--thread-accent': accent } as CSSProperties}>
      <button
        className="thread-task-head"
        onClick={() => setIsExpanded((expanded) => !expanded)}
        type="button"
      >
        {running ? (
          <span className="thread-task-spinner" aria-label="running" />
        ) : (
          <span className="thread-task-check" aria-label="done">
            ✓
          </span>
        )}
        <span className="thread-task-title">{card.title}</span>
        <span className="thread-task-count">
          {visibleCount} {visibleCount === 1 ? 'tool call' : 'tool calls'}
        </span>
        <span className="thread-task-chev">{isExpanded ? '▴' : '▾'}</span>
      </button>
      {isExpanded && (
        <div className="thread-task-body">
          {card.items.slice(0, visibleCount).map((item) => (
            <div className="thread-task-item" key={item.command}>
              <div className="thread-task-command-label">Run command:</div>
              <div>
                <code className="thread-task-command">{item.command}</code>
                <pre className="thread-task-output">
                  <code>{item.output}</code>
                </pre>
                <div className="thread-task-exit">exit code {item.exitCode ?? 0}</div>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

function StreamingBubble({
  text,
  speed,
  onDone,
}: {
  text: string
  speed: number
  onDone: () => void
}) {
  const words = useMemo(() => text.split(/(\s+)/), [text])
  const [count, setCount] = useState(0)
  const startedAt = useRef(0)
  const raf = useRef(0)
  const done = useRef(false)

  useEffect(() => {
    startedAt.current = performance.now()
    done.current = false
    setCount(0)

    function tick(now: number) {
      const elapsed = (now - startedAt.current) / 1000
      const realWordsTotal = words.filter((word) => word.trim().length).length
      const targetReal = Math.min(realWordsTotal, Math.floor(elapsed * speed))
      let realSeen = 0
      let tokenIdx = 0

      while (tokenIdx < words.length && realSeen < targetReal) {
        tokenIdx += 1
        if (words[tokenIdx - 1].trim().length) realSeen += 1
      }

      while (tokenIdx < words.length && !words[tokenIdx].trim().length) {
        tokenIdx += 1
      }

      setCount(tokenIdx)

      if (tokenIdx < words.length) {
        raf.current = requestAnimationFrame(tick)
        return
      }

      if (!done.current) {
        done.current = true
        onDone()
      }
    }

    raf.current = requestAnimationFrame(tick)
    return () => cancelAnimationFrame(raf.current)
  }, [onDone, speed, text, words])

  const shown = words.slice(0, count).join('')
  const remaining = words.slice(count).join('')
  const isDone = count >= words.length

  return (
    <span className="thread-panel-stream">
      {shown}
      {!isDone && <span className="thread-panel-caret" />}
      {/*
        Render the un-streamed remainder invisible so the bubble reserves its
        final height from frame one. The scroll-on-phase-change effect can
        then settle on a scrollHeight that already accounts for the full
        message, and the bottom edge stops clipping as words come in.
       */}
      {!isDone && (
        <span className="thread-panel-stream-ghost" aria-hidden="true">
          {remaining}
        </span>
      )}
    </span>
  )
}

function renderHumanText(text: string, botName: string) {
  const parts = text.includes('#')
    ? text.split(/(#[a-z0-9-]+)/gi).map((part, index) =>
        part.startsWith('#') ? (
          <span key={`${part}-${index}`} className="thread-panel-mention">
            {part}
          </span>
        ) : (
          <span key={`${part}-${index}`}>{part}</span>
        ),
      )
    : [<span key="text">{text}</span>]

  return (
    <>
      <span className="thread-panel-mention">@{botName}</span>
      <span> </span>
      {parts}
    </>
  )
}

function ThreadDetail({
  accent,
  botGlyph,
  botName,
  speed,
  thread,
}: {
  accent: string
  botGlyph: string
  botName: string
  speed: number
  thread: ThreadData
}) {
  const [replyIdx, setReplyIdx] = useState(-1)
  const [phase, setPhase] = useState<'pending' | 'typing' | 'stream' | 'shown' | 'done'>(
    'pending',
  )
  const scrollRef = useRef<HTMLDivElement | null>(null)
  const stickToBottomRef = useRef(true)

  useEffect(() => {
    stickToBottomRef.current = true
    const id = window.setTimeout(() => {
      setReplyIdx(0)
      setPhase(thread.replies[0]?.from === 'bot' ? 'typing' : 'shown')
    }, 520)
    return () => window.clearTimeout(id)
  }, [thread.id, thread.replies])

  useEffect(() => {
    if (replyIdx < 0) return

    let timer: number | undefined
    if (phase === 'typing') {
      timer = window.setTimeout(() => setPhase('stream'), 780)
    } else if (phase === 'shown' && thread.replies[replyIdx]?.from === 'user') {
      timer = window.setTimeout(() => {
        if (replyIdx + 1 < thread.replies.length) {
          const next = thread.replies[replyIdx + 1]
          setReplyIdx(replyIdx + 1)
          setPhase(next.from === 'bot' ? 'typing' : 'shown')
        }
      }, 600)
    }

    return () => {
      if (timer) window.clearTimeout(timer)
    }
  }, [replyIdx, phase, thread.replies])

  const onStreamDone = useCallback(() => {
    window.setTimeout(() => {
      if (replyIdx + 1 < thread.replies.length) {
        const next = thread.replies[replyIdx + 1]
        setReplyIdx(replyIdx + 1)
        setPhase(next.from === 'bot' ? 'typing' : 'shown')
      } else {
        setPhase('done')
      }
    }, 560)
  }, [replyIdx, thread.replies])

  const handleScroll = useCallback(() => {
    const element = scrollRef.current
    if (!element) return
    const distanceFromBottom = element.scrollHeight - element.scrollTop - element.clientHeight
    stickToBottomRef.current = distanceFromBottom < 48
  }, [])

  useEffect(() => {
    const element = scrollRef.current
    if (!element) return
    if (!stickToBottomRef.current) return
    element.scrollTop = element.scrollHeight
  }, [phase, replyIdx, thread.id])

  const visible: Array<Reply & { i: number; isTyping: boolean; isStreaming: boolean }> = []
  for (let i = 0; i <= replyIdx && i < thread.replies.length; i += 1) {
    const reply = thread.replies[i]
    const isCurrent = i === replyIdx
    visible.push({
      i,
      ...reply,
      isTyping: isCurrent && reply.from === 'bot' && phase === 'typing',
      isStreaming: isCurrent && reply.from === 'bot' && phase === 'stream',
    })
  }

  const shownReplies = visible.filter((reply) => !reply.isTyping).length
  const firstBotReplyIndex = visible.find((reply) => reply.from === 'bot')?.i
  const accumulatedTaskCard = visible.reduce<TaskCardData | undefined>((card, reply) => {
    if (reply.from !== 'bot' || !reply.card) return card
    if (!card) return { ...reply.card, items: [...reply.card.items] }
    return { ...card, items: [...card.items, ...reply.card.items] }
  }, undefined)
  const isTaskCardRunning = visible.some(
    (reply) => reply.from === 'bot' && Boolean(reply.card) && (reply.isStreaming || reply.isTyping),
  )

  return (
    <section className="thread-panel-detail" aria-label={`Thread in ${thread.channel}`}>
      <header className="thread-panel-head">
        <div className="thread-panel-head-titles">
          <div className="thread-panel-title">{thread.title}</div>
          <div className="thread-panel-sub">
            <span>Thread</span>
            <span className="thread-panel-head-sep" aria-hidden="true">·</span>
            <span className="thread-panel-head-channel"># {thread.channel}</span>
          </div>
        </div>
      </header>

      <div className="thread-panel-scroll" onScroll={handleScroll} ref={scrollRef}>
        <div className="thread-panel-parent">
          <div className="thread-panel-row">
            <div className="thread-panel-avatar" style={{ background: thread.parent.color }}>
              {thread.parent.glyph}
            </div>
            <div>
              <div className="thread-panel-msg-head">
                <span className="thread-panel-name">{thread.parent.who}</span>
                <span className="thread-panel-time">{thread.parent.time}</span>
              </div>
              <div className="thread-panel-body">{renderHumanText(thread.parent.body, botName)}</div>
            </div>
          </div>
        </div>

        <div className="thread-panel-rule">
          <span>
            {shownReplies} {shownReplies === 1 ? 'reply' : 'replies'}
          </span>
          <span className="thread-panel-line" />
        </div>

        {visible.map((reply) => (
          <div
            className={`thread-panel-msg ${
              reply.from === 'bot' ? 'thread-panel-msg-bot' : 'thread-panel-msg-user'
            }`}
            key={`${thread.id}-${reply.i}`}
          >
            {reply.from === 'bot' ? (
              <BotAvatar glyph={botGlyph} accent={accent} />
            ) : (
              <div
                className="thread-panel-avatar thread-panel-avatar-user"
                style={reply.color ? { background: reply.color } : undefined}
              >
                {reply.glyph ?? thread.parent.glyph}
              </div>
            )}
            <div>
              <div className="thread-panel-msg-head">
                <span
                  className={reply.from === 'bot' ? 'thread-panel-name-bot' : 'thread-panel-name'}
                >
                  {reply.from === 'bot' ? botName : (reply.who ?? thread.parent.who)}
                </span>
                {reply.from === 'bot' && <span className="thread-panel-badge">App</span>}
                <span className="thread-panel-time">
                  {reply.time}
                  {reply.isStreaming || reply.isTyping ? ' · just now' : ''}
                </span>
              </div>
              <div className="thread-panel-body">
                {reply.from === 'bot' &&
                  reply.i === firstBotReplyIndex &&
                  accumulatedTaskCard && (
                  <TaskCard
                    accent={accent}
                    card={accumulatedTaskCard}
                    running={isTaskCardRunning}
                  />
                )}
                {reply.isTyping ? (
                  <TypingDots />
                ) : reply.isStreaming ? (
                  <StreamingBubble text={reply.text} speed={speed} onDone={onStreamDone} />
                ) : reply.from === 'user' ? (
                  renderHumanText(reply.text, botName)
                ) : (
                  <span>{reply.text}</span>
                )}
              </div>
            </div>
          </div>
        ))}
      </div>

    </section>
  )
}

export default function ThreadPanel({
  accent = '#28c26a',
  speed = 34,
  botName = 'Centaur',
  botGlyph = 'C',
}: {
  accent?: string
  speed?: number
  botName?: string
  botGlyph?: string
}) {
  const [selected, setSelected] = useState(0)
  const activeThread = threadData[selected]

  return (
    <div className="thread-panel" style={{ '--thread-accent': accent } as CSSProperties}>
      <aside className="thread-list" aria-label="Threads">
        <div className="thread-list-head">
          <div className="thread-list-title">Threads</div>
        </div>
        <div className="thread-list-items">
          {threadData.map((thread, index) => {
            const isActive = index === selected

            return (
              <button
                className={`thread-list-item ${isActive ? 'thread-list-item-active' : ''}`}
                key={thread.id}
                onClick={() => setSelected(index)}
                style={{ '--thread-accent': accent } as CSSProperties}
                type="button"
              >
                <div className="thread-list-channel-row">
                  <span className="thread-list-channel">
                    <span className="thread-list-channel-label"># {thread.channel}</span>
                    {isActive && <span className="thread-list-live-dot" />}
                  </span>
                  <span className="thread-list-time">{thread.replies.at(-1)?.time}</span>
                </div>
                <div className="thread-list-name">{thread.title}</div>
              </button>
            )
          })}
        </div>
      </aside>

      <ThreadDetail
        accent={accent}
        botGlyph={botGlyph}
        botName={botName}
        key={activeThread.id}
        speed={speed}
        thread={activeThread}
      />
    </div>
  )
}
