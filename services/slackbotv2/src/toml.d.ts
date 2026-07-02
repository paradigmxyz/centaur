// Bun imports TOML files natively (used for harness/codex/config.toml); this
// declaration makes the type checker accept them.
declare module '*.toml' {
  const value: Record<string, unknown>
  export default value
}
