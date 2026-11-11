// Where the auth pages may send the browser.
//
// The server allowlists every path it returns (ConsoleReturnPath); this is a
// second line of defence so a same-origin target can never turn into an
// off-origin one ("//host", "/\host", "javascript:...").
export function safeLocalPath(path: string | null | undefined, fallback = "/"): string {
  if (!path || !path.startsWith("/") || path.startsWith("//")) return fallback;
  if (/[\\\s]/.test(path) || [...path].some((c) => c.charCodeAt(0) < 0x20 || c.charCodeAt(0) === 0x7f)) return fallback;
  return path;
}

/** A broker OAuth consent start path (the result page's "Try again" link). */
export function isOauthStartPath(path: string | null | undefined): path is string {
  return typeof path === "string" && /^\/oauth\/[A-Za-z0-9_-]+\/start$/.test(path);
}
