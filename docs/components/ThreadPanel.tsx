'use client'

import type { CSSProperties, ReactNode } from 'react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'

type Reply = {
  from: 'bot' | 'user'
  text: string
  time: string
  card?: TaskCardData
  table?: TableData
  who?: string
  glyph?: string
  color?: string
  avatar?: string
}

type TaskCardData = {
  title: string
  items: Array<{
    command: string
    output: string
    exitCode?: number
  }>
}

type TableData = {
  headers: string[]
  rows: string[][]
}

const personalizedQuestionsTable: TableData = {
  headers: ['Who', 'Questions'],
  rows: [
    [
      '*MatX: Reiner Pope / Mike Gunter*',
      '“What workload shape makes MatX most advantaged: training, prefill, decode, RL, or MoE routing?” “Does putting weights in SRAM reduce total HBM demand, or does it free HBM to support longer KV/context?” “What becomes binding in a MatX cluster: SRAM capacity, HBM bandwidth, HBM capacity, scale-up interconnect, power, or software?”',
    ],
    [
      '*Xiaoyu Ma / David Patterson*',
      '“If you had to rank 2027 bottlenecks, how would you order HBM bandwidth, HBM capacity, interconnect latency, packaging, and power?” “Which proposed fix is closest to production: HBF, PNM, 3D memory-logic stacking, CXL/memory pooling, or low-latency interconnect?” “What would falsify the ‘memory not compute’ inference thesis?”',
    ],
    [
      '*TurboQuant: Amir Zandieh / Vahab Mirrokni*',
      '“In production, does KV compression reduce HBM capacity purchased, or mostly increase max context/concurrency?” “Where does TurboQuant break: model size, MoE, multimodal, very long context, latency budgets, or kernel integration?” “How much of the claimed gain survives in vLLM/SGLang/TensorRT-LLM with real batching and scheduling?”',
    ],
    [
      '*vLLM / SGLang / TensorRT-LLM maintainers*',
      '“For decode today, where do tokens actually stall: HBM bandwidth, KV fragmentation, attention kernels, scheduler overhead, interconnect, or CPU orchestration?” “What optimizations have mattered most in real deployments over the last 12 months?” “If hardware gave you 2x HBM bandwidth, 2x HBM capacity, or 2x interconnect, which improves $/token most?”',
    ],
    [
      '*Hyperscaler inference infra leads*',
      '“What is the binding procurement constraint for 2026-27: GPUs, HBM, networking, power, racks, cooling, or deployment labor?” “Are you buying more memory per accelerator because of longer context/agents, or using compression/offload to hold memory flat?” “How do you split spend between Nvidia GPUs, custom ASICs, storage, and networking for inference?”',
    ],
    [
      '*HBM / packaging architects*',
      '“What is the true HBM bottleneck: DRAM wafer starts, TSV stacking, base die, packaging, test, substrate/interposer, or CoWoS capacity?” “How much can HBM4 supply realistically expand in 2027 without hurting yields?” “Are HBM margins high enough that suppliers will reallocate commodity DRAM wafer capacity, or is packaging the limiter?”',
    ],
    [
      '*NAND / HBF experts*',
      '“Is HBF a real 2027-28 product path or a standards/narrative exercise?” “What workloads can tolerate NAND-like latency if bandwidth and capacity improve?” “Does KV compression reduce the need for HBF, or does long-context/memory-tiering still create demand?”',
    ],
  ],
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
    avatar?: string
    time: string
    body: string
  }
  replies: Reply[]
}

const threadData: ThreadData[] = [
  {
    id: 'memory-stocks',
    channel: 'ai-agent',
    title: 'Compare AI memory stocks',
    parent: {
      who: 'Matt Huang',
      glyph: 'M',
      color: '#28c26a',
      avatar: '/avatars/matt-huang.jpg',
      time: 'Today at 10:13 AM',
      body: '@centaur_ai Compare the top HBM, NAND, DRAM, HDD stocks based on 2027 estimates',
    },
    replies: [
      {
        from: 'bot',
        text: 'Top screen on 2027E earnings: *SK Hynix and Samsung are cheapest, SanDisk has the highest NAND beta, Micron is the cleanest U.S. HBM/DRAM liquid play, and HDD looks more expensive despite strong fundamentals.*\n\nSources: StockAnalysis consensus for MU, SNDK, STX, WDC; MarketScreener for SK Hynix and Samsung; Tom’s Hardware/Bloomberg summary for SK Hynix HBM share/capacity context. (stockanalysis.com)\n\nMy ranking for a 2027 memory/storage basket: *1. SK Hynix, 2. Samsung, 3. Micron, 4. SanDisk, 5. WDC, 6. Seagate*. If you want purity over valuation: *SanDisk for NAND, SK Hynix for HBM, Micron for U.S. HBM/DRAM, Seagate for HDD*.',
        time: '10:13 AM',
      },
      {
        from: 'user',
        text: 'Makes sense. @centaur_ai Where are the bottlenecks looking like based on latest published papers in top tier conferences, as well as what ppl are saying on X?',
        time: '10:21 AM',
        who: 'Georgios Konstantopoulos',
        glyph: 'G',
        color: '#0f766e',
        avatar: '/avatars/georgios.jpg',
      },
      {
        from: 'bot',
        text: '@georgios The bottleneck is shifting from “can we get GPUs?” to *memory bandwidth/capacity + data movement*, especially for inference.\n\nBottom line: *HBM bandwidth is the cleanest bottleneck; KV cache capacity is the most actively attacked bottleneck; NAND/HBF is the interesting optionality; HDD is a capacity-cycle story, not the core model-runtime bottleneck.* That keeps my preference tilted toward *SK Hynix/Micron/Samsung for bottleneck purity*, with *SanDisk as the convex HBF/NAND bet* and *STX/WDC as storage-cycle plays*.\n\nSources include ICLR 2026 TurboQuant, AAAI 2026 KV prefetching, 2026 LLM inference hardware/HBM-PIM papers, and recent X posts discussing HBM/KV cache constraints. (openreview.net)',
        time: '10:21 AM',
      },
      {
        from: 'user',
        text: '@centaur_ai Does any of this analysis change based on new AI chips like MatX?',
        time: '10:24 AM',
        who: 'Matt Huang',
        glyph: 'M',
        color: '#28c26a',
        avatar: '/avatars/matt-huang.jpg',
      },
      {
        from: 'bot',
        text: 'Yes, but it *reinforces* the prior analysis more than overturning it.\n\nMatX is explicitly designed around the same bottleneck: *keep weights in SRAM, use HBM mostly for KV cache, and add heavy scale-up interconnect for MoE/long-context workloads*. That means chips like MatX reduce _wasted_ HBM traffic, but they do not remove HBM from the stack; they make HBM bandwidth more targeted toward KV/context. MatX itself says weights are typically in SRAM and KVs in HBM, while CEO Reiner Pope says the design spends “none” of HBM bandwidth loading weights and instead uses it for KV cache. (matx.com) (chipstrat.com)\n\nInvestment read-through: *slightly less bullish for generic GPU incumbency, still bullish HBM, more bullish SRAM/on-chip memory/interconnect-aware architectures, and still only optional for NAND/HBF*. For the memory stock basket, I would not change the ranking much: SK Hynix/Micron/Samsung remain the bottleneck-pure plays; MatX-style designs actually validate that memory/interconnect, not raw FLOPs, are the constraint.',
        time: '10:24 AM',
      },
      {
        from: 'user',
        text: '@centaur_ai Who should we go talk to to get a better read? Ideally people that are more technical who can give us a nuanced understanding.',
        time: '10:26 AM',
        who: 'Georgios Konstantopoulos',
        glyph: 'G',
        color: '#0f766e',
        avatar: '/avatars/georgios.jpg',
      },
      {
        from: 'bot',
        text: '@georgios I’d prioritize *technical operators/researchers over equity analysts*: people who can explain where tokens stall in real serving systems.\n\nKey diligence questions: “For decode, what percent of time is HBM bandwidth vs interconnect vs scheduler overhead?” “Does KV compression reduce HBM purchases or just enable longer contexts/more users?” “What part of HBM supply is actually binding?” “Do MoE models make scale-up interconnect the next bottleneck?” Sources: MatX interview/site, Ma/Patterson inference hardware paper, TurboQuant authors/paper.',
        time: '10:26 AM',
      },
      {
        from: 'user',
        text: '@centaur_ai Let’s personalize the questions for each.',
        time: '10:28 AM',
        who: 'Georgios Konstantopoulos',
        glyph: 'G',
        color: '#0f766e',
        avatar: '/avatars/georgios.jpg',
      },
      {
        from: 'bot',
        text: '@georgios Here’s a personalized question set by expert type.',
        table: personalizedQuestionsTable,
        time: '10:28 AM',
      },
    ],
  },
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

function Avatar({
  avatar,
  color,
  glyph,
}: {
  avatar?: string
  color: string
  glyph: string
}) {
  return (
    <div className="thread-panel-avatar" style={{ background: color }}>
      {avatar ? <img className="thread-panel-avatar-img" src={avatar} alt="" /> : glyph}
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
      {renderSlackText(shown)}
      {!isDone && <span className="thread-panel-caret" />}
      {/*
        Render the un-streamed remainder invisible so the bubble reserves its
        final height from frame one. The scroll-on-phase-change effect can
        then settle on a scrollHeight that already accounts for the full
        message, and the bottom edge stops clipping as words come in.
       */}
      {!isDone && (
        <span className="thread-panel-stream-ghost" aria-hidden="true">
          {renderSlackText(remaining)}
        </span>
      )}
    </span>
  )
}

function renderSlackInline(text: string, keyPrefix: string): ReactNode[] {
  const tokenPattern = /(<https?:\/\/[^|>]+(?:\|[^>]+)?>|@[a-z0-9_]+|\*[^*\n]+\*|_[^_\n]+_)/gi
  const nodes: ReactNode[] = []
  let lastIndex = 0

  for (const match of text.matchAll(tokenPattern)) {
    const token = match[0]
    const index = match.index ?? 0
    if (index > lastIndex) nodes.push(text.slice(lastIndex, index))

    if (token.startsWith('<http')) {
      const [url, label] = token.slice(1, -1).split('|')
      nodes.push(
        <span
          className="thread-panel-link"
          key={`${keyPrefix}-link-${index}`}
        >
          {label || url}
        </span>,
      )
    } else if (token.startsWith('@')) {
      nodes.push(
        <span className="thread-panel-mention" key={`${keyPrefix}-mention-${index}`}>
          {token}
        </span>,
      )
    } else if (token.startsWith('*')) {
      nodes.push(<strong key={`${keyPrefix}-strong-${index}`}>{token.slice(1, -1)}</strong>)
    } else if (token.startsWith('_')) {
      nodes.push(<em key={`${keyPrefix}-em-${index}`}>{token.slice(1, -1)}</em>)
    } else {
      nodes.push(token)
    }

    lastIndex = index + token.length
  }

  if (lastIndex < text.length) nodes.push(text.slice(lastIndex))
  return nodes
}

function renderSlackText(text: string): ReactNode {
  return <>{renderSlackInline(text, text.slice(0, 16))}</>
}

function renderHumanText(text: string, botName: string) {
  const body = text.trimStart().startsWith('@')
    ? text
    : `@${botName}${text.startsWith(' ') ? '' : ' '}${text}`
  return renderSlackText(body)
}

function SlackTable({ table }: { table: TableData }) {
  return (
    <div className="thread-table-wrap">
      <table className="thread-table">
        <thead>
          <tr>
            {table.headers.map((header) => (
              <th key={header}>{renderSlackText(header)}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {table.rows.map((row, rowIndex) => (
            <tr key={`${row[0]}-${rowIndex}`}>
              {row.map((cell, cellIndex) => (
                <td key={`${cellIndex}-${cell.slice(0, 16)}`}>{renderSlackText(cell)}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
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
            <Avatar
              avatar={thread.parent.avatar}
              color={thread.parent.color}
              glyph={thread.parent.glyph}
            />
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
              <Avatar
                avatar={reply.avatar}
                color={reply.color ?? thread.parent.color}
                glyph={reply.glyph ?? thread.parent.glyph}
              />
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
                  <>
                    {renderSlackText(reply.text)}
                    {reply.table && <SlackTable table={reply.table} />}
                  </>
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
          <img className="thread-list-brand" src="/paradigm-mark.svg" alt="" aria-hidden="true" />
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
