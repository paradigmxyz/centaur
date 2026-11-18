//! OAuth area tests: apps, broker credentials, the MCP authorization
//! server, the broker consent flows, and Slack enrichment. DB tests skip
//! without `CONSOLE_DB_TEST_DATABASE_URL`.

mod apps;
mod credentials;
mod enrich;
mod flows;
mod mcp;
mod support;
