const INTERNAL_DELIVERY_PATH_RE =
  /^\/api\/slack\/(?:agent-sessions(?:\/|$)|assistant\/(?:status|title)$)/

export function usesShortRequestTimeout(path: string): boolean {
  return !INTERNAL_DELIVERY_PATH_RE.test(path)
}
