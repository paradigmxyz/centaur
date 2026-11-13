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

export interface Principal {
  readonly id: string;
  readonly email?: string;
  readonly name?: string;
  /** Live OAuth providers, such as "slack" or "google". */
  readonly oauth: readonly string[];
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
   * The environment value for a secret, if set. Behind iron-proxy this is a
   * placeholder the proxy swaps on the wire; inject-mode secrets need none.
   */
  secret(name: string): string | undefined;
  readonly principal: Principal;
  readonly files: Files;
  sleep(ms: number): Promise<void>;
}
