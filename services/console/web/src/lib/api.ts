// The SPA talks only to the session-only console API under /api/v1/console
// (CONSOLE_API_PREFIX); apiRequest refuses any other path, so it never calls
// the bearer API. It sends no CSRF token: the browser supplies the same-origin
// Origin header, which the server checks exactly on every write.

export type ErrorDetails = Record<string, unknown> & { reason?: string };

export class ApiError extends Error {
  readonly status: number;
  readonly details: ErrorDetails | undefined;

  constructor(status: number, message: string, details?: ErrorDetails) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.details = details;
  }

  get reason(): string | undefined {
    return typeof this.details?.reason === "string" ? this.details.reason : undefined;
  }

  /** Per-field validation messages from a 422 `details` hash. */
  get fieldErrors(): Record<string, string[]> {
    if (this.status !== 422 || !this.details) return {};
    const out: Record<string, string[]> = {};
    for (const [key, value] of Object.entries(this.details)) {
      if (Array.isArray(value)) out[key] = value.map(String);
    }
    return out;
  }
}

export interface PageMeta {
  page: number;
  limit: number;
  total: number;
  total_pages: number;
}

export interface DataEnvelope<T> {
  data: T;
}

export interface ListEnvelope<T> {
  data: T[];
  meta: PageMeta;
}

export type QueryValue = string | number | boolean | null | undefined | readonly (string | number)[];

export interface RequestOptions {
  method?: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
  query?: Record<string, QueryValue>;
  body?: unknown;
  signal?: AbortSignal;
}

/** Every SPA request goes here; /api/v1 outside it is the bearer API. */
export const CONSOLE_API_PREFIX = "/api/v1/console/";

/** Hook for tests: where full-page redirects (login, pending) go. */
export const navigation = {
  assign(url: string) {
    window.location.assign(url);
  },
};

/** The server honours return_to only for same-origin paths (api-rs console/return_path.rs). */
export function loginUrl(location: Pick<Location, "pathname" | "search"> = window.location): string {
  const here = location.pathname + location.search;
  return `/login?return_to=${encodeURIComponent(here)}`;
}

export function buildUrl(path: string, query?: Record<string, QueryValue>): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query ?? {})) {
    if (value === undefined || value === null || value === "") continue;
    if (Array.isArray(value)) {
      for (const item of value) params.append(`${key}[]`, String(item));
    } else {
      params.set(key, String(value));
    }
  }
  const qs = params.toString();
  return qs ? `${path}?${qs}` : path;
}

/** The same-origin console API URL for `path`, checked after normalization ("..", encoded dots, backslashes). */
function consoleApiUrl(path: string, query?: Record<string, QueryValue>): string {
  const url = new URL(buildUrl(path, query), window.location.origin);
  if (url.origin !== window.location.origin || !url.pathname.startsWith(CONSOLE_API_PREFIX)) {
    throw new Error(`The console SPA only calls ${CONSOLE_API_PREFIX}*; refused ${path}`);
  }
  return url.pathname + url.search;
}

export async function apiRequest<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const url = consoleApiUrl(path, options.query);
  const method = options.method ?? "GET";
  const headers: Record<string, string> = { Accept: "application/json" };
  if (options.body !== undefined) headers["Content-Type"] = "application/json";

  const response = await fetch(url, {
    method,
    headers,
    credentials: "same-origin",
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
    signal: options.signal,
  });

  if (response.status === 204) return undefined as T;

  const text = await response.text();
  let body: unknown = undefined;
  if (text) {
    try {
      body = JSON.parse(text);
    } catch {
      body = undefined;
    }
  }

  if (response.ok) return body as T;

  const envelope = (body as { error?: { message?: string; details?: ErrorDetails } } | undefined)?.error;
  const error = new ApiError(
    response.status,
    envelope?.message ?? `Request failed with HTTP ${response.status}`,
    envelope?.details,
  );

  if (error.status === 401) {
    navigation.assign(loginUrl());
  } else if (error.status === 403 && error.reason === "account_pending") {
    navigation.assign("/pending");
  }
  throw error;
}

export const api = {
  get: <T>(path: string, query?: Record<string, QueryValue>, signal?: AbortSignal) =>
    apiRequest<T>(path, { query, signal }),
  post: <T>(path: string, body?: unknown) => apiRequest<T>(path, { method: "POST", body: body ?? {} }),
  put: <T>(path: string, body?: unknown) => apiRequest<T>(path, { method: "PUT", body: body ?? {} }),
  patch: <T>(path: string, body?: unknown) => apiRequest<T>(path, { method: "PATCH", body: body ?? {} }),
  delete: <T = void>(path: string) => apiRequest<T>(path, { method: "DELETE" }),
};

/** Human message for any thrown value. */
export function errorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    const fields = error.fieldErrors;
    const first = Object.entries(fields)[0];
    if (error.status === 422 && first) {
      return first[0] === "base" ? (first[1][0] ?? error.message) : `${humanize(first[0])} ${first[1][0] ?? ""}`.trim();
    }
    return error.message;
  }
  if (error instanceof Error) return error.message;
  return "Something went wrong.";
}

export function humanize(key: string): string {
  const words = key.replace(/_id$/, " ID").replace(/_/g, " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
}
