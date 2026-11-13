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

export interface RequestInit {
  method?: string;
  headers?: Record<string, string | undefined>;
  body?: string | Uint8Array | ArrayBuffer;
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
  principal: { id: string; foreign_id: string; name: string };
  capabilities: Record<string, boolean>;
  oauth_credentials: OAuthCredential[];
  connected_tools?: string[];
  [key: string]: unknown;
}

/**
 * The sandbox context file (`~/.centaur/sandbox.json`) that the platform writes for the
 * session. Informational: route on it, never authorize anything with it; iron-proxy and
 * Console enforce access.
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

/** The principal's scratch directory. Paths are relative, without `..`. */
export interface Files {
  read(path: string): Promise<Uint8Array>;
  readText(path: string): Promise<string>;
  write(path: string, data: string | Uint8Array): Promise<{ path: string; bytes: number }>;
}

export interface Ctx {
  fetch(url: string, init?: RequestInit): Promise<Response>;
  /**
   * The runner's environment, read once at startup: configuration such as API
   * base URLs or database proxy endpoints. It never holds real credentials;
   * where a credential variable is set it is a placeholder iron-proxy swaps on
   * the wire, and inject-mode credentials need no variable at all.
   */
  readonly env: Readonly<Record<string, string | undefined>>;
  /** The calling context, read once per cell; `null` outside a Centaur session. */
  readonly context: Readonly<CallingContext> | null;
  readonly files: Files;
  sleep(ms: number): Promise<void>;
}
