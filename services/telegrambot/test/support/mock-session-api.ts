// The fake api-rs session API, ported nearly verbatim from discordbot's
// chat-sdk-emulate harness (itself ported from slackbotv2).
import {
  createServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";
import type { ServerNotification } from "@centaur/harness-events";
import type {
  TelegrambotAppendMessagesRequest,
  TelegrambotCreateSessionRequest,
  TelegrambotExecuteSessionRequest,
} from "../../src/index";
import { availablePort, closeServer, listen } from "./net";

export function sampleCodexNotifications(
  answer: string | string[],
): ServerNotification[] {
  const deltas = typeof answer === "string" ? [answer] : answer;
  return [
    {
      method: "turn/started",
      params: {
        threadId: "thread-1",
        turn: {
          id: "turn-1",
          items: [],
          itemsView: "full",
          status: "inProgress",
          error: null,
          startedAt: 1,
          completedAt: null,
          durationMs: null,
        },
      },
    },
    {
      method: "item/started",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        startedAtMs: 2,
        item: {
          type: "agentMessage",
          id: "commentary-1",
          text: "",
          phase: "commentary",
          memoryCitation: null,
        },
      },
    },
    {
      method: "item/started",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        startedAtMs: 4,
        item: {
          type: "agentMessage",
          id: "answer-1",
          text: "",
          phase: "final_answer",
          memoryCitation: null,
        },
      },
    },
    {
      method: "item/agentMessage/delta",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        itemId: "commentary-1",
        delta: "Checking the command output",
      },
    },
    {
      method: "item/reasoning/summaryTextDelta",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        itemId: "reasoning-1",
        summaryIndex: 0,
        delta: "Inspecting the event stream",
      },
    },
    {
      method: "item/completed",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        completedAtMs: 2,
        item: {
          type: "agentMessage",
          id: "commentary-1",
          text: "Checking the command output",
          phase: "commentary",
          memoryCitation: null,
        },
      },
    },
    {
      method: "turn/plan/updated",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        explanation: "Implementation plan",
        plan: [
          { step: "Inspect App Server events", status: "completed" },
          { step: "Stream Chat SDK chunks", status: "inProgress" },
        ],
      },
    },
    {
      method: "item/started",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        startedAtMs: 2,
        item: {
          type: "commandExecution",
          id: "cmd-1",
          command: "pnpm test",
          cwd: "/repo",
          processId: "proc-1",
          source: "agent",
          status: "inProgress",
          commandActions: [],
          aggregatedOutput: null,
          exitCode: null,
          durationMs: null,
        },
      },
    },
    {
      method: "item/commandExecution/outputDelta",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        itemId: "cmd-1",
        delta: "tests passed\n",
      },
    },
    {
      method: "item/completed",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        completedAtMs: 3,
        item: {
          type: "commandExecution",
          id: "cmd-1",
          command: "pnpm test",
          cwd: "/repo",
          processId: "proc-1",
          source: "agent",
          status: "completed",
          commandActions: [],
          aggregatedOutput: "tests passed\n",
          exitCode: 0,
          durationMs: 50,
        },
      },
    },
    ...deltas.map((delta) => ({
      method: "item/agentMessage/delta",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        itemId: "answer-1",
        delta,
      },
    })),
  ] as unknown as ServerNotification[];
}

export function sampleCodexOutputLines(answer: string | string[]): string[] {
  return [
    ...sampleCodexNotifications(answer).map((notification) =>
      JSON.stringify(notification),
    ),
    JSON.stringify({
      type: "turn.completed",
      turn: { id: "turn-1", items: [] },
    }),
  ];
}

export type MockSessionRequest<T> = {
  body: T;
  threadKey: string;
};

type MockSessionEventRequest = {
  afterEventId: number;
  executionId?: string;
  threadKey: string;
};

export type MockSessionEvent = {
  data: string;
  event: string;
  executionId?: string;
  id: number;
  threadKey: string;
};

export type MockSessionApi = {
  appends: MockSessionRequest<TelegrambotAppendMessagesRequest>[];
  autoRespond: boolean;
  close(): Promise<void>;
  closeStreams(): void;
  creates: MockSessionRequest<TelegrambotCreateSessionRequest>[];
  emitOutputLine(threadKey: string, line: string, executionId?: string): void;
  emitOutputLines(
    threadKey: string,
    lines: string[],
    executionId?: string,
  ): void;
  emitSessionEvent(
    threadKey: string,
    event: string,
    data: unknown,
    executionId?: string,
  ): void;
  eventRequests: MockSessionEventRequest[];
  executes: MockSessionRequest<TelegrambotExecuteSessionRequest>[];
  failAllEvents: boolean;
  failNextCreate: boolean;
  /** Fail this many upcoming creates with `createFailureStatus`. */
  failCreates: number;
  createFailureStatus: number;
  failNextEvents: boolean;
  failNextExecute: boolean;
  failNextExecuteAfterAccept: boolean;
  hasStream(threadKey: string): boolean;
  holdNextExecute(): () => void;
  reset(): void;
  streamCount: number;
  url: string;
};

export async function startMockCodexApi(): Promise<MockSessionApi> {
  const appends: MockSessionRequest<TelegrambotAppendMessagesRequest>[] = [];
  const creates: MockSessionRequest<TelegrambotCreateSessionRequest>[] = [];
  const eventRequests: MockSessionEventRequest[] = [];
  const events: MockSessionEvent[] = [];
  const executes: MockSessionRequest<TelegrambotExecuteSessionRequest>[] = [];
  const idempotentExecutions = new Map<string, string>();
  const streams = new Set<ServerResponse>();
  const streamThreadKeys = new Map<ServerResponse, string>();
  let autoRespond = true;
  let executeHold: Promise<void> | null = null;
  let executeHoldRelease: (() => void) | null = null;
  let eventId = 0;
  let failAllEvents = false;
  let failNextCreate = false;
  let failCreates = 0;
  let createFailureStatus = 503;
  let failNextEvents = false;
  let failNextExecute = false;
  let failNextExecuteAfterAccept = false;
  const port = await availablePort(4163);
  const closeStreams = () => {
    for (const stream of streams) stream.end();
    streams.clear();
    streamThreadKeys.clear();
  };
  const server = createServer((req, res) => {
    void handleMockCodexRequest(req, res, {
      appends,
      creates,
      events,
      eventRequests,
      executes,
      get autoRespond() {
        return autoRespond;
      },
      get executeHold() {
        return executeHold;
      },
      get failAllEvents() {
        return failAllEvents;
      },
      get failNextCreate() {
        return failNextCreate;
      },
      takeCreateFailure() {
        if (failCreates <= 0) return undefined;
        failCreates -= 1;
        return createFailureStatus;
      },
      get failNextEvents() {
        return failNextEvents;
      },
      get failNextExecute() {
        return failNextExecute;
      },
      get failNextExecuteAfterAccept() {
        return failNextExecuteAfterAccept;
      },
      idempotentExecutions,
      nextEventId() {
        eventId += 1;
        return eventId;
      },
      port,
      setFailNextCreate(value) {
        failNextCreate = value;
      },
      setFailNextEvents(value) {
        failNextEvents = value;
      },
      setFailNextExecute(value) {
        failNextExecute = value;
      },
      setFailNextExecuteAfterAccept(value) {
        failNextExecuteAfterAccept = value;
      },
      streams,
      streamThreadKeys,
    }).catch((error) => {
      res.writeHead(500, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: String(error) }));
    });
  });
  await listen(server, port);

  const api: MockSessionApi = {
    appends,
    creates,
    eventRequests,
    executes,
    reset() {
      appends.length = 0;
      creates.length = 0;
      eventRequests.length = 0;
      events.length = 0;
      executes.length = 0;
      idempotentExecutions.clear();
      executeHoldRelease?.();
      executeHold = null;
      executeHoldRelease = null;
      closeStreams();
      autoRespond = true;
      eventId = 0;
      failAllEvents = false;
      failNextCreate = false;
      failCreates = 0;
      createFailureStatus = 503;
      failNextEvents = false;
      failNextExecute = false;
      failNextExecuteAfterAccept = false;
    },
    url: `http://127.0.0.1:${port}`,
    closeStreams,
    get autoRespond() {
      return autoRespond;
    },
    set autoRespond(value: boolean) {
      autoRespond = value;
    },
    get failAllEvents() {
      return failAllEvents;
    },
    set failAllEvents(value: boolean) {
      failAllEvents = value;
    },
    get failNextCreate() {
      return failNextCreate;
    },
    set failNextCreate(value: boolean) {
      failNextCreate = value;
    },
    get failCreates() {
      return failCreates;
    },
    set failCreates(value: number) {
      failCreates = value;
    },
    get createFailureStatus() {
      return createFailureStatus;
    },
    set createFailureStatus(value: number) {
      createFailureStatus = value;
    },
    get failNextEvents() {
      return failNextEvents;
    },
    set failNextEvents(value: boolean) {
      failNextEvents = value;
    },
    get failNextExecute() {
      return failNextExecute;
    },
    set failNextExecute(value: boolean) {
      failNextExecute = value;
    },
    get failNextExecuteAfterAccept() {
      return failNextExecuteAfterAccept;
    },
    set failNextExecuteAfterAccept(value: boolean) {
      failNextExecuteAfterAccept = value;
    },
    holdNextExecute() {
      if (executeHoldRelease) throw new Error("execute is already held");
      executeHold = new Promise((resolve) => {
        executeHoldRelease = resolve;
      });
      return () => {
        const release = executeHoldRelease;
        executeHoldRelease = null;
        executeHold = null;
        release?.();
      };
    },
    get streamCount() {
      return streams.size;
    },
    // streamCount counts LIVE streams across all threads; a stream from the
    // previous test that has not closed yet can satisfy a bare count wait.
    // Order-sensitive tests wait for THEIR thread's stream instead.
    hasStream(threadKey: string) {
      return Array.from(streamThreadKeys.values()).includes(threadKey);
    },
    emitOutputLine(threadKey: string, line: string, executionId?: string) {
      emitMockSessionEvent({
        data: line,
        event: "session.output.line",
        executionId,
        events,
        id: ++eventId,
        streams,
        threadKey,
      });
    },
    emitOutputLines(threadKey: string, lines: string[], executionId?: string) {
      for (const line of lines)
        api.emitOutputLine(threadKey, line, executionId);
    },
    emitSessionEvent(
      threadKey: string,
      event: string,
      data: unknown,
      executionId?: string,
    ) {
      emitMockSessionEvent({
        data: typeof data === "string" ? data : JSON.stringify(data),
        event,
        executionId,
        events,
        id: ++eventId,
        streams,
        threadKey,
      });
    },
    async close() {
      closeStreams();
      await closeServer(server);
    },
  };
  return api;
}

async function handleMockCodexRequest(
  req: IncomingMessage,
  res: ServerResponse,
  input: {
    appends: MockSessionRequest<TelegrambotAppendMessagesRequest>[];
    autoRespond: boolean;
    creates: MockSessionRequest<TelegrambotCreateSessionRequest>[];
    events: MockSessionEvent[];
    eventRequests: MockSessionEventRequest[];
    executeHold: Promise<void> | null;
    executes: MockSessionRequest<TelegrambotExecuteSessionRequest>[];
    failAllEvents: boolean;
    failNextCreate: boolean;
    failNextEvents: boolean;
    failNextExecute: boolean;
    failNextExecuteAfterAccept: boolean;
    idempotentExecutions: Map<string, string>;
    nextEventId(): number;
    port: number;
    setFailNextCreate(value: boolean): void;
    takeCreateFailure(): number | undefined;
    setFailNextEvents(value: boolean): void;
    setFailNextExecute(value: boolean): void;
    setFailNextExecuteAfterAccept(value: boolean): void;
    streams: Set<ServerResponse>;
    streamThreadKeys: Map<ServerResponse, string>;
  },
): Promise<void> {
  const url = new URL(req.url ?? "/", `http://127.0.0.1:${input.port}`);
  const match =
    /^\/api\/session\/([^/]+)(?:\/(messages|execute|events))?$/.exec(
      url.pathname,
    );
  if (!match?.[1]) {
    await sendWebResponse(res, new Response("not found", { status: 404 }));
    return;
  }
  const threadKey = decodeURIComponent(match[1]);
  const endpoint = match[2] ?? "session";

  if (endpoint === "session") {
    const request = await nodeRequestToWebRequest(req, url);
    const body = (await request.json()) as TelegrambotCreateSessionRequest;
    input.creates.push({ threadKey, body });
    const failureStatus = input.takeCreateFailure();
    if (failureStatus !== undefined) {
      await sendWebResponse(
        res,
        new Response("create failed", {
          status: failureStatus,
          statusText:
            failureStatus >= 500 ? "Service Unavailable" : "Bad Request",
        }),
      );
      return;
    }
    if (input.failNextCreate) {
      input.setFailNextCreate(false);
      await sendWebResponse(
        res,
        new Response("unavailable", {
          status: 503,
          statusText: "Service Unavailable",
        }),
      );
      return;
    }
    await sendWebResponse(
      res,
      Response.json({
        thread_key: threadKey,
        sandbox_id: null,
        harness_type: body.harness_type,
        harness_thread_id: null,
        status: "active",
      }),
    );
    return;
  }

  if (endpoint === "events") {
    const afterEventId =
      Number.parseInt(url.searchParams.get("after_event_id") ?? "0", 10) || 0;
    const executionId = url.searchParams.get("execution_id") || undefined;
    input.eventRequests.push({ threadKey, afterEventId, executionId });
    if (input.failAllEvents || input.failNextEvents) {
      input.setFailNextEvents(false);
      await sendWebResponse(
        res,
        new Response("unavailable", {
          status: 503,
          statusText: "Service Unavailable",
        }),
      );
      return;
    }
    res.writeHead(200, {
      "cache-control": "no-cache",
      connection: "keep-alive",
      "content-type": "text/event-stream",
    });
    input.streams.add(res);
    input.streamThreadKeys.set(res, threadKey);
    for (const event of input.events) {
      if (
        event.threadKey === threadKey &&
        event.id > afterEventId &&
        (!executionId ||
          !event.executionId ||
          event.executionId === executionId)
      ) {
        writeMockSseEvent(res, event);
      }
    }
    req.once("close", () => {
      input.streams.delete(res);
      input.streamThreadKeys.delete(res);
    });
    return;
  }

  const request = await nodeRequestToWebRequest(req, url);
  if (endpoint === "messages") {
    const body = (await request.json()) as TelegrambotAppendMessagesRequest;
    input.appends.push({ threadKey, body });
    await sendWebResponse(
      res,
      Response.json({
        ok: true,
        message_ids: body.messages.map((_, index) => `msg-${index + 1}`),
      }),
    );
    return;
  }

  const body = (await request.json()) as TelegrambotExecuteSessionRequest;
  input.executes.push({ threadKey, body });
  if (input.failNextExecute) {
    input.setFailNextExecute(false);
    await sendWebResponse(
      res,
      new Response("unavailable", {
        status: 503,
        statusText: "Service Unavailable",
      }),
    );
    return;
  }
  if (input.executeHold) await input.executeHold;
  const idempotencyMapKey = body.idempotency_key
    ? `${threadKey}:${body.idempotency_key}`
    : undefined;
  const existingExecutionId = idempotencyMapKey
    ? input.idempotentExecutions.get(idempotencyMapKey)
    : undefined;
  const executionId =
    existingExecutionId ??
    `exe-${input.idempotentExecutions.size + input.executes.length}`;
  if (idempotencyMapKey && !existingExecutionId) {
    input.idempotentExecutions.set(idempotencyMapKey, executionId);
  }
  if (!existingExecutionId && input.autoRespond) {
    for (const line of sampleCodexOutputLines(
      `Executed request ${input.idempotentExecutions.size}.`,
    )) {
      emitMockSessionEvent({
        data: line,
        event: "session.output.line",
        executionId,
        events: input.events,
        id: input.nextEventId(),
        streams: input.streams,
        threadKey,
      });
    }
  }
  if (input.failNextExecuteAfterAccept) {
    input.setFailNextExecuteAfterAccept(false);
    await sendWebResponse(
      res,
      new Response("response lost after accept", {
        status: 503,
        statusText: "Service Unavailable",
      }),
    );
    return;
  }
  await sendWebResponse(
    res,
    Response.json({
      ok: true,
      execution_id: executionId,
      thread_key: threadKey,
      status: "completed",
    }),
  );
}

function emitMockSessionEvent(input: {
  data: string;
  event: string;
  executionId?: string;
  events: MockSessionEvent[];
  id: number;
  streams: Set<ServerResponse>;
  threadKey: string;
}): void {
  const event: MockSessionEvent = {
    data: input.data,
    event: input.event,
    executionId: input.executionId,
    id: input.id,
    threadKey: input.threadKey,
  };
  input.events.push(event);
  for (const stream of input.streams) writeMockSseEvent(stream, event);
}

function writeMockSseEvent(
  stream: ServerResponse,
  event: MockSessionEvent,
): void {
  stream.write(`id: ${event.id}\n`);
  stream.write(`event: ${event.event}\n`);
  for (const line of event.data.split("\n")) {
    stream.write(`data: ${line}\n`);
  }
  stream.write("\n");
}

async function nodeRequestToWebRequest(
  req: IncomingMessage,
  url: URL,
): Promise<Request> {
  const headers = new Headers();
  for (const [key, value] of Object.entries(req.headers)) {
    if (Array.isArray(value)) {
      for (const item of value) headers.append(key, item);
    } else if (typeof value === "string") {
      headers.set(key, value);
    }
  }

  const chunks: Buffer[] = [];
  for await (const chunk of req) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }
  const body = Buffer.concat(chunks);
  return new Request(url, {
    body:
      body.length > 0 && req.method !== "GET" && req.method !== "HEAD"
        ? body
        : undefined,
    headers,
    method: req.method,
  });
}

async function sendWebResponse(
  res: ServerResponse,
  response: Response,
): Promise<void> {
  res.statusCode = response.status;
  res.statusMessage = response.statusText;
  response.headers.forEach((value, key) => {
    res.setHeader(key, value);
  });
  if (response.body === null || response.status === 204) {
    res.end();
    return;
  }
  res.end(Buffer.from(await response.arrayBuffer()));
}
