// Types the Code Mode runtime (crates/centaur-codemode) provides to connectors.
// Connectors import them with `import type { Ctx } from "../centaur";`.

/** Subset of the Fetch API. Requests go through the principal's iron-proxy. */
export interface Response {
  readonly status: number;
  readonly ok: boolean;
  /** Lowercase header names; repeated headers are joined with ", ". */
  readonly headers: Record<string, string>;
  text(): Promise<string>;
  json(): Promise<any>;
  bytes(): Promise<Uint8Array>;
}

/** A file under the files root, sent as part of a request body without reading it into the realm. */
export interface FilePart {
  file: string;
}

export type BodyPart = string | Uint8Array | ArrayBuffer | FilePart;

export interface RequestInit {
  method?: string;
  headers?: Record<string, string | undefined>;
  /** One part, or parts sent in order (for example a multipart body around a `{ file }`). */
  body?: BodyPart | BodyPart[];
  /**
   * How long this request may take, body included, up to 600000 (10 minutes). By default a
   * request gets the call's remaining time (a call has 60 s). A longer timeout extends the
   * call's and the cell's deadlines to fit, so set it only for transfers that need it, such as
   * large downloads and uploads.
   */
  timeout_ms?: number;
}

/** A file the runtime read or wrote. */
export interface FileInfo {
  /** Absolute path, as the agent's other tools open it. */
  path: string;
  bytes: number;
}

export interface Download {
  status: number;
  ok: boolean;
  /** Lowercase header names. */
  headers: Record<string, string>;
  /** The saved file, when `ok`. */
  path?: string;
  bytes?: number;
  /** The start of the response body when not `ok`; nothing is saved then. */
  body?: string;
}

/**
 * Read and write files under the runner's files directory (in a Centaur sandbox, the agent's
 * workspace). Paths are relative to it, or absolute paths inside it; parent directories are
 * created; writes replace existing files. Files are at most 100 MiB.
 */
export interface Files {
  read(path: string): Promise<Uint8Array>;
  readText(path: string): Promise<string>;
  write(path: string, data: string | Uint8Array | ArrayBuffer): Promise<FileInfo>;
  /** `null` when nothing exists at `path`. */
  stat(path: string): Promise<FileInfo | null>;
  /**
   * Request `url` and stream a 2xx body straight to `path`, without the 10 MiB response limit
   * or passing the bytes through the realm. Recorded like `fetch`. Pass `init.timeout_ms` for
   * files that may take longer than the call's remaining time to transfer.
   */
  download(url: string, path: string, init?: RequestInit): Promise<Download>;
}

export interface OAuthCredential {
  slug: string;
  /** Such as "slack" or "google". */
  provider: string;
  /** "live" when usable. */
  status: string;
  provider_email?: string | null;
  scopes?: string[];
}

/** Console's `GET /api/v1/sandbox/permissions` data for the calling principal. */
export interface SandboxPermissions {
  principal_id: string;
  /** `kind` is Console's principal kind, such as `slack_channel`, `slack_dm` or `console_user` (MCP). */
  principal: { id: string; foreign_id: string; name: string; kind?: string };
  capabilities: Record<string, boolean>;
  oauth_credentials: OAuthCredential[];
  connected_tools?: string[];
  [key: string]: unknown;
}

/**
 * The calling context, read once per cell. In a session sandbox it is the sandbox context file
 * (`~/.centaur/sandbox.json`) the platform writes. In a Code Mode sandbox, which has no such file,
 * it comes from Console's sandbox permissions, with `harness: "codemode"` and a null `thread_key`.
 * Informational: route on it, never authorize anything with it; iron-proxy and Console enforce
 * access.
 */
export interface CallingContext {
  version?: number;
  thread_key?: string | null;
  harness?: string | null;
  persona?: string | null;
  /** `null` when Console could not be reached; `sandbox_error` says why. */
  sandbox?: SandboxPermissions | null;
  sandbox_error?: string;
  [key: string]: unknown;
}

/**
 * A text WebSocket owned by the current connector call. It closes when the call ends, so connect,
 * talk and finish within one call; keep longer-lived state upstream. Messages are at most 10 MiB.
 */
export interface WebSocketConn {
  send(text: string): Promise<void>;
  /**
   * The next message, or `null` when none arrives within `timeout_ms` (default: the call's
   * remaining time). Throws once the socket has closed.
   */
  recv(timeout_ms?: number): Promise<string | null>;
  close(): Promise<void>;
}

export interface Ctx {
  fetch(url: string, init?: RequestInit): Promise<Response>;
  /**
   * Open a `ws://` or `wss://` WebSocket through the principal's iron-proxy, recorded against this
   * call like `fetch`. A call may make at most 4 attempts; `timeout_ms` bounds the handshake.
   */
  websocket(url: string, init?: { timeout_ms?: number }): Promise<WebSocketConn>;
  /**
   * The runner's environment, read once at startup: configuration such as API
   * base URLs or database proxy endpoints. It never holds real credentials;
   * where a credential variable is set it is a placeholder iron-proxy swaps on
   * the wire, and inject-mode credentials need no variable at all.
   */
  readonly env: Readonly<Record<string, string | undefined>>;
  /**
   * The calling context, read once per cell; `null` when there is no context file and Code Mode
   * has no Console permissions.
   */
  readonly context: Readonly<CallingContext> | null;
  sleep(ms: number): Promise<void>;
  /**
   * The files capability, or `null` when the runner has none (outside a Centaur sandbox).
   * Functions that produce or consume files should require it (see `_lib/files`) rather than
   * pass file contents as base64.
   */
  readonly files: Files | null;
  /** Base64 in the standard alphabet; `url: true` encodes URL-safe without padding. */
  readonly base64: {
    encode(data: string | Uint8Array | ArrayBuffer, options?: { url?: boolean }): string;
    /** Accepts both alphabets, with or without padding. */
    decode(text: string): Uint8Array;
    /** `decode`, then UTF-8. */
    decodeText(text: string): string;
  };
  /** Gzip-compress bytes (strings are UTF-8 encoded). */
  gzip(data: string | Uint8Array | ArrayBuffer): Uint8Array;
  /**
   * Decompress gzip (all members) or zlib data, up to 10 MiB. HTTP responses with
   * `Content-Encoding: gzip` or `deflate` are already decompressed by `fetch`.
   */
  gunzip(data: Uint8Array | ArrayBuffer): Uint8Array;
}
