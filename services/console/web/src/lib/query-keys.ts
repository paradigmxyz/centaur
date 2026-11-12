// Query key conventions. Every feature owns one root key (its slice id) and
// builds keys through createQueryKeys so invalidation by prefix works:
//
//   const keys = createQueryKeys("principals");
//   keys.all                 -> ["principals"]
//   keys.list({ q, page })   -> ["principals", "list", { q, page }]
//   keys.detail(id)          -> ["principals", "detail", id]
//   keys.sub(id, "grants", { page }) -> ["principals", "detail", id, "grants", { page }]
//
// After a write, invalidate the narrowest prefix that covers what changed,
// e.g. queryClient.invalidateQueries({ queryKey: keys.detail(id) }).

export function createQueryKeys<const Root extends string>(root: Root) {
  return {
    all: [root] as const,
    lists: () => [root, "list"] as const,
    list: (params: Record<string, unknown> = {}) => [root, "list", params] as const,
    details: () => [root, "detail"] as const,
    detail: (id: string) => [root, "detail", id] as const,
    sub: (id: string, name: string, params: Record<string, unknown> = {}) =>
      [root, "detail", id, name, params] as const,
  };
}

/** Keys owned by the shell / shared components. */
export const sharedKeys = {
  me: ["me"] as const,
  secrets: (params: Record<string, unknown> = {}) => ["secrets", "list", params] as const,
  slackChannels: (params: Record<string, unknown> = {}) => ["slack-channels", params] as const,
};
