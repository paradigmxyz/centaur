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

/**
 * The OAuth app slug format: OauthApp::URL_SAFE_FORMAT without its anchors
 * (A-Z, a-z, 0-9, "-", ".", "_", "~"). Keep in sync; a Rails test
 * (console_return_path_test.rb) compares the two.
 */
export const OAUTH_SLUG_SOURCE = "[A-Za-z0-9\\-._~]+";
const OAUTH_START_PATH = new RegExp(`^/oauth/(${OAUTH_SLUG_SOURCE})/start$`);

/** A broker OAuth consent start path (the result page's "Try again" link). */
export function isOauthStartPath(path: string | null | undefined): path is string {
  if (typeof path !== "string") return false;
  const slug = OAUTH_START_PATH.exec(path)?.[1];
  // "." and ".." are URL-safe but would be resolved as dot segments.
  return slug !== undefined && slug !== "." && slug !== "..";
}
