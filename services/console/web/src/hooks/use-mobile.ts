import { useSyncExternalStore } from "react"

const MOBILE_BREAKPOINT = 768
const QUERY = `(max-width: ${MOBILE_BREAKPOINT - 1}px)`

// useSyncExternalStore instead of the generated effect + state, so the first
// render already knows the viewport (no desktop flash on phones).
export function useIsMobile() {
  return useSyncExternalStore(
    (onChange) => {
      const list = window.matchMedia?.(QUERY)
      list?.addEventListener?.("change", onChange)
      return () => list?.removeEventListener?.("change", onChange)
    },
    () => window.matchMedia?.(QUERY).matches ?? false,
    () => false
  )
}
