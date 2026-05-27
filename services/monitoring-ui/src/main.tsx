import React, { useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  Activity,
  AlertTriangle,
  BarChart3,
  CheckCircle2,
  Clock3,
  Database,
  FileSearch,
  ListFilter,
  RefreshCw,
  Search,
  TerminalSquare,
  Trophy,
  Users,
  Wrench,
} from "lucide-react";
import "./styles.css";

type WindowKey = "24h" | "7d" | "30d";
type TabKey = "overview" | "leaderboard" | "executions" | "tools" | "thread";

type Overview = {
  live: Record<string, number>;
  totals: {
    executions: number;
    completed: number;
    failed: number;
    total_tokens: number;
    cost_usd: number;
    tool_calls: number;
    tool_errors: number;
  };
  series: Array<{ bucket: string; executions: number; completed: number; failed: number; cost_usd: number }>;
  top_tools: Array<{ tool_name: string; calls: number }>;
};

type Leader = {
  user_id: string;
  executions: number;
  completed: number;
  failed: number;
  total_tokens: number;
  cost_usd: number;
  tool_calls: number;
  tool_errors: number;
  last_activity_at: string | null;
};

type Execution = {
  execution_id: string;
  thread_key: string;
  created_at: string;
  status: string;
  terminal_reason: string | null;
  harness: string | null;
  engine: string | null;
  persona_id: string | null;
  user_id: string | null;
  duration_s: number | null;
  total_tokens: number;
  cost_usd: number;
  tool_calls: number;
  tool_errors: number;
  models: string[];
  tool_calls_by_name: Record<string, number>;
};

type ToolEvent = {
  use_event_id: number;
  result_event_id: number;
  thread_key: string;
  execution_id: string;
  user_id: string | null;
  tool_use_id: string;
  tool_name: string;
  input_keys: string[];
  input_size_bytes: number;
  use_created_at: string;
  result_status: "success" | "error" | "pending";
  error_category: string | null;
  content_size_bytes: number;
};

type ThreadDetail = {
  thread_key: string;
  session: null | Record<string, string | null>;
  messages: Record<string, number | string | null>;
  executions: Execution[];
  timeline: Array<Record<string, unknown>>;
};

function fmtNumber(value: number | null | undefined): string {
  return new Intl.NumberFormat().format(Number(value ?? 0));
}

function fmtMoney(value: number | null | undefined): string {
  return new Intl.NumberFormat(undefined, { style: "currency", currency: "USD", maximumFractionDigits: 4 }).format(
    Number(value ?? 0),
  );
}

function fmtDate(value: string | null | undefined): string {
  if (!value) return "n/a";
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}

async function getJson<T>(path: string): Promise<T> {
  const response = await fetch(path);
  if (!response.ok) {
    const detail = await response.text();
    throw new Error(detail || `Request failed with ${response.status}`);
  }
  return response.json() as Promise<T>;
}

function useResource<T>(path: string, deps: React.DependencyList) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string>("");
  const [loading, setLoading] = useState(true);
  const [nonce, setNonce] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError("");
    getJson<T>(path)
      .then((next) => {
        if (!cancelled) setData(next);
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [...deps, nonce]);

  return { data, error, loading, refresh: () => setNonce((value) => value + 1) };
}

function Badge({ children, tone = "neutral" }: { children: React.ReactNode; tone?: "neutral" | "good" | "bad" | "warn" }) {
  return <span className={`badge badge-${tone}`}>{children}</span>;
}

function StatusBadge({ status }: { status: string | null | undefined }) {
  const resolved = status ?? "unknown";
  const tone = resolved === "completed" || resolved === "success" ? "good" : resolved === "running" ? "warn" : resolved === "queued" ? "neutral" : "bad";
  return <Badge tone={tone}>{resolved}</Badge>;
}

function StatCard({ label, value, icon: Icon }: { label: string; value: string; icon: React.ComponentType<{ size?: number }> }) {
  return (
    <div className="stat-card">
      <div>
        <div className="stat-label">{label}</div>
        <div className="stat-value">{value}</div>
      </div>
      <Icon size={22} />
    </div>
  );
}

function DataState({ loading, error, children }: { loading: boolean; error: string; children: React.ReactNode }) {
  if (loading) return <div className="state">Loading monitoring data...</div>;
  if (error) return <div className="state state-error">{error}</div>;
  return <>{children}</>;
}

function OverviewTab({ windowKey }: { windowKey: WindowKey }) {
  const { data, error, loading } = useResource<Overview>(`/api/monitoring/overview?window=${windowKey}`, [windowKey]);
  const peak = useMemo(() => Math.max(1, ...(data?.series.map((item) => item.executions) ?? [1])), [data]);

  return (
    <DataState loading={loading} error={error}>
      {data && (
        <div className="stack">
          <div className="stats-grid">
            <StatCard label="Executions" value={fmtNumber(data.totals.executions)} icon={Activity} />
            <StatCard label="Active sessions" value={fmtNumber(data.live.active_sessions)} icon={Clock3} />
            <StatCard label="Tokens" value={fmtNumber(data.totals.total_tokens)} icon={Database} />
            <StatCard label="Cost" value={fmtMoney(data.totals.cost_usd)} icon={BarChart3} />
            <StatCard label="Tool calls" value={fmtNumber(data.totals.tool_calls)} icon={Wrench} />
            <StatCard label="Tool errors" value={fmtNumber(data.totals.tool_errors)} icon={AlertTriangle} />
          </div>
          <div className="columns">
            <section className="panel">
              <div className="panel-title">Execution Trend</div>
              <div className="bars">
                {data.series.length === 0 && <div className="empty">No executions in this window.</div>}
                {data.series.map((point) => (
                  <div className="bar-row" key={point.bucket}>
                    <div className="bar-label">{fmtDate(point.bucket)}</div>
                    <div className="bar-track">
                      <div className="bar-fill" style={{ width: `${Math.max(4, (point.executions / peak) * 100)}%` }} />
                    </div>
                    <div className="bar-count">{point.executions}</div>
                  </div>
                ))}
              </div>
            </section>
            <section className="panel">
              <div className="panel-title">Top Tools</div>
              <div className="list">
                {data.top_tools.length === 0 && <div className="empty">No tool calls in this window.</div>}
                {data.top_tools.map((tool) => (
                  <div className="list-row" key={tool.tool_name}>
                    <span>{tool.tool_name}</span>
                    <Badge>{fmtNumber(tool.calls)}</Badge>
                  </div>
                ))}
              </div>
            </section>
          </div>
        </div>
      )}
    </DataState>
  );
}

function LeaderboardTab({ windowKey }: { windowKey: WindowKey }) {
  const { data, error, loading } = useResource<{ items: Leader[] }>(`/api/monitoring/leaderboard?window=${windowKey}`, [windowKey]);
  return (
    <DataState loading={loading} error={error}>
      <Table
        columns={["User", "Execs", "Complete", "Failed", "Tokens", "Cost", "Tools", "Errors", "Last active"]}
        empty={!data?.items.length}
      >
        {data?.items.map((row) => (
          <tr key={row.user_id}>
            <td>{row.user_id}</td>
            <td>{fmtNumber(row.executions)}</td>
            <td>{fmtNumber(row.completed)}</td>
            <td>{fmtNumber(row.failed)}</td>
            <td>{fmtNumber(row.total_tokens)}</td>
            <td>{fmtMoney(row.cost_usd)}</td>
            <td>{fmtNumber(row.tool_calls)}</td>
            <td>{fmtNumber(row.tool_errors)}</td>
            <td>{fmtDate(row.last_activity_at)}</td>
          </tr>
        ))}
      </Table>
    </DataState>
  );
}

function ExecutionsTab({ windowKey, onThread }: { windowKey: WindowKey; onThread: (thread: string) => void }) {
  const { data, error, loading } = useResource<{ items: Execution[] }>(`/api/monitoring/executions?window=${windowKey}`, [windowKey]);
  return (
    <DataState loading={loading} error={error}>
      <Table columns={["Status", "User", "Thread", "Engine", "Duration", "Tokens", "Cost", "Tools", "When"]} empty={!data?.items.length}>
        {data?.items.map((row) => (
          <tr key={row.execution_id}>
            <td><StatusBadge status={row.status} /></td>
            <td>{row.user_id ?? "unknown"}</td>
            <td><button className="link-button" onClick={() => onThread(row.thread_key)}>{row.thread_key}</button></td>
            <td>{[row.harness, row.engine, row.persona_id].filter(Boolean).join(" / ")}</td>
            <td>{row.duration_s == null ? "n/a" : `${row.duration_s}s`}</td>
            <td>{fmtNumber(row.total_tokens)}</td>
            <td>{fmtMoney(row.cost_usd)}</td>
            <td>{fmtNumber(row.tool_calls)} <span className="muted">({fmtNumber(row.tool_errors)} err)</span></td>
            <td>{fmtDate(row.created_at)}</td>
          </tr>
        ))}
      </Table>
    </DataState>
  );
}

function ToolsTab({ windowKey, onThread }: { windowKey: WindowKey; onThread: (thread: string) => void }) {
  const [errorsOnly, setErrorsOnly] = useState(false);
  const path = `/api/monitoring/tool-events?window=${windowKey}&errors_only=${errorsOnly ? "true" : "false"}`;
  const { data, error, loading } = useResource<{ items: ToolEvent[] }>(path, [windowKey, errorsOnly]);
  return (
    <div className="stack">
      <div className="toolbar">
        <label className="toggle">
          <input type="checkbox" checked={errorsOnly} onChange={(event) => setErrorsOnly(event.target.checked)} />
          Errors only
        </label>
      </div>
      <DataState loading={loading} error={error}>
        <Table columns={["Result", "Tool", "Input keys", "Sizes", "User", "Thread", "When"]} empty={!data?.items.length}>
          {data?.items.map((row) => (
            <tr key={row.use_event_id}>
              <td><StatusBadge status={row.result_status} /></td>
              <td>{row.tool_name}</td>
              <td><div className="chips">{row.input_keys.map((key) => <Badge key={key}>{key}</Badge>)}</div></td>
              <td>{fmtNumber(row.input_size_bytes)} in / {fmtNumber(row.content_size_bytes)} out</td>
              <td>{row.user_id ?? "unknown"}</td>
              <td><button className="link-button" onClick={() => onThread(row.thread_key)}>{row.thread_key}</button></td>
              <td>{fmtDate(row.use_created_at)}</td>
            </tr>
          ))}
        </Table>
      </DataState>
    </div>
  );
}

function ThreadTab({ threadKey }: { threadKey: string }) {
  const encoded = encodeURIComponent(threadKey);
  const { data, error, loading } = useResource<ThreadDetail>(threadKey ? `/api/monitoring/threads/${encoded}` : "/api/monitoring/threads/none", [threadKey]);
  if (!threadKey) {
    return <div className="state">Select a thread from Executions or Tool Compliance.</div>;
  }
  return (
    <DataState loading={loading} error={error}>
      {data && (
        <div className="stack">
          <section className="panel">
            <div className="panel-title">{data.thread_key}</div>
            <div className="stats-grid compact">
              <StatCard label="Messages" value={fmtNumber(Number(data.messages.message_count))} icon={Users} />
              <StatCard label="Attachments" value={fmtNumber(Number(data.messages.attachment_count))} icon={FileSearch} />
              <StatCard label="Executions" value={fmtNumber(data.executions.length)} icon={TerminalSquare} />
              <StatCard label="State" value={String(data.session?.state ?? "no session")} icon={Activity} />
            </div>
          </section>
          <section className="panel">
            <div className="panel-title">Timeline</div>
            <div className="timeline">
              {data.timeline.map((item) => (
                <div className="timeline-row" key={String(item.event_id)}>
                  <div><Badge>{String(item.event_kind)}</Badge></div>
                  <div>{fmtDate(String(item.created_at))}</div>
                  <pre>{JSON.stringify(item, null, 2)}</pre>
                </div>
              ))}
            </div>
          </section>
        </div>
      )}
    </DataState>
  );
}

function Table({ columns, empty, children }: { columns: string[]; empty: boolean; children: React.ReactNode }) {
  return (
    <div className="table-wrap">
      <table>
        <thead>
          <tr>{columns.map((column) => <th key={column}>{column}</th>)}</tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
      {empty && <div className="empty table-empty">No rows for this selection.</div>}
    </div>
  );
}

function App() {
  const [tab, setTab] = useState<TabKey>("overview");
  const [windowKey, setWindowKey] = useState<WindowKey>("24h");
  const [threadKey, setThreadKey] = useState("");
  const tabs: Array<{ key: TabKey; label: string; icon: React.ComponentType<{ size?: number }> }> = [
    { key: "overview", label: "Overview", icon: Activity },
    { key: "leaderboard", label: "Leaderboard", icon: Trophy },
    { key: "executions", label: "Executions", icon: ListFilter },
    { key: "tools", label: "Tool Compliance", icon: Wrench },
    { key: "thread", label: "Thread", icon: Search },
  ];

  const openThread = (thread: string) => {
    setThreadKey(thread);
    setTab("thread");
  };

  return (
    <main>
      <header>
        <div className="brand-lockup">
          <img
            src="https://cdn.prod.website-files.com/646e2e8c6fc42a55d153a7c9/6470cc6f27e8ba00d8380736_Logo.png"
            alt="Percents"
          />
          <div>
            <div className="eyebrow"><CheckCircle2 size={14} /> Private network</div>
            <h1><span>Centaur</span> Monitoring</h1>
          </div>
        </div>
        <div className="segmented">
          {(["24h", "7d", "30d"] as WindowKey[]).map((value) => (
            <button key={value} className={windowKey === value ? "active" : ""} onClick={() => setWindowKey(value)}>{value}</button>
          ))}
        </div>
      </header>
      <nav>
        {tabs.map((item) => {
          const Icon = item.icon;
          return (
            <button key={item.key} className={tab === item.key ? "active" : ""} onClick={() => setTab(item.key)}>
              <Icon size={16} /> {item.label}
            </button>
          );
        })}
        <button className="icon-button" onClick={() => location.reload()} title="Refresh"><RefreshCw size={16} /></button>
      </nav>
      {tab === "overview" && <OverviewTab windowKey={windowKey} />}
      {tab === "leaderboard" && <LeaderboardTab windowKey={windowKey} />}
      {tab === "executions" && <ExecutionsTab windowKey={windowKey} onThread={openThread} />}
      {tab === "tools" && <ToolsTab windowKey={windowKey} onThread={openThread} />}
      {tab === "thread" && <ThreadTab threadKey={threadKey} />}
    </main>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
