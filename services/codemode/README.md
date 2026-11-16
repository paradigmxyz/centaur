# Code Mode

`centaur-codemode` is the experimental Code Mode runtime api-rs starts for each
principal when `CENTAUR_MCP_V2_ENABLED=true`. This image carries only the
`centaur-codemode` binary and the repository connectors baked at
`/opt/centaur/connectors`; it has no `/entrypoint.sh`. It runs as `agent`
(uid/gid 1001), trusts the proxy CA through `CENTAUR_CODEMODE_CA_FILE`, and
serves the HTTP API in this crate's README. See `crates/centaur-codemode` for
the runtime, CLI, and connector documentation.
