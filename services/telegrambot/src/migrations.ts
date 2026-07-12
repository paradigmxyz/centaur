import type { Pool, PoolClient } from "pg";
import type { Logger } from "./types";
import { noopLogger } from "./utils";

/**
 * Application-startup migration runner for the Telegram-owned tables
 * (spec §2a). The runtime role is also the migration runner: this service has
 * no chart migration job, and the tables are purpose-built for this service
 * alone, so startup DDL keeps the deployment story identical to discordbot
 * (readiness simply stays false until migrations have applied).
 *
 * Migrations are embedded ordered SQL strings — no filesystem reads, so the
 * container image and the code can never disagree about schema content.
 *
 * Rollback behavior: migrations are additive and forward-compatible; there
 * are no down migrations. Rolling a deployment back one release leaves the
 * newer schema in place, which older additive-compatible code can normally
 * still use — but readiness fails closed on a schema version the running
 * build does not know (`unsupported_future`), so rolling back across a
 * schema-version boundary is an explicit operator decision (deploy a build
 * that supports the version, or intervene manually), never a silent one.
 */

/**
 * Advisory-lock key serializing concurrent migration runs (two pods racing a
 * rollout, or startup racing a manual run). Constant, service-scoped:
 * 0x74656c65 is ASCII "tele" — stable across releases so every telegrambot
 * build contends on the same lock.
 */
const MIGRATION_LOCK_KEY = 0x74656c65;

const MIGRATIONS: readonly string[] = [
  // 1: durable receipt ledger + fenced poll state.
  //   - telegram_poll_state: one row per bot user id (never token-derived, so
  //     token rotation preserves state); carries the committed receive_offset
  //     and the fencing lease (holder_id, generation, lease_expires_at).
  //   - telegram_update_inbox: every update returned by getUpdates and its
  //     processing stage, keyed (bot_user_id, update_id). render_obligation
  //     holds the durable TelegramRenderObligation — renderer state rides on
  //     the inbox row of the triggering update so obligation persistence is a
  //     fenced stage transition like any other.
  //   - status index for claim/recovery scans; a partial nonterminal index
  //     because recovery scans only care about in-flight rows while the table
  //     is dominated by terminal rows awaiting retention pruning.
  `
  CREATE TABLE IF NOT EXISTS telegram_poll_state (
    bot_user_id text PRIMARY KEY,
    receive_offset bigint,
    holder_id text,
    generation bigint NOT NULL DEFAULT 0,
    lease_expires_at timestamptz
  );

  CREATE TABLE IF NOT EXISTS telegram_update_inbox (
    bot_user_id text NOT NULL,
    update_id bigint NOT NULL,
    payload jsonb NOT NULL,
    status text NOT NULL DEFAULT 'received',
    thread_key text,
    client_message_id text,
    execution_id text,
    status_reason text,
    render_obligation jsonb,
    received_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (bot_user_id, update_id)
  );

  CREATE INDEX IF NOT EXISTS telegram_update_inbox_status_idx
    ON telegram_update_inbox (bot_user_id, status, update_id);

  CREATE INDEX IF NOT EXISTS telegram_update_inbox_nonterminal_idx
    ON telegram_update_inbox (bot_user_id, update_id)
    WHERE status NOT IN ('completed', 'steered', 'ignored', 'rejected', 'failed');
  `,
];

/** Highest schema version this build can run against. */
export const SUPPORTED_SCHEMA_VERSION = MIGRATIONS.length;

export type SchemaVersionState = "ok" | "pending" | "unsupported_future";

export type SchemaVersionCheck = {
  ok: boolean;
  state: SchemaVersionState;
  dbVersion: number;
  supportedVersion: number;
};

export async function runMigrations(
  pool: Pool,
  logger?: Logger,
): Promise<void> {
  const log = logger ?? noopLogger;
  // Dedicated connection: pg_advisory_lock is session-scoped, so lock and
  // unlock must run on the same physical connection, never through the pool's
  // per-query client rotation.
  const client = await pool.connect();
  let destroy: Error | undefined;
  try {
    await client.query("SELECT pg_advisory_lock($1)", [MIGRATION_LOCK_KEY]);
    try {
      await applyPending(client, log);
    } finally {
      try {
        await client.query("SELECT pg_advisory_unlock($1)", [
          MIGRATION_LOCK_KEY,
        ]);
      } catch (unlockError) {
        // Unlock failed => connection state unknown; destroy it so the
        // advisory lock is released by connection close, not leaked into a
        // recycled pool client.
        destroy =
          unlockError instanceof Error ? unlockError : new Error("unlock");
      }
    }
  } finally {
    client.release(destroy);
  }
}

async function applyPending(client: PoolClient, log: Logger): Promise<void> {
  await client.query(`
    CREATE TABLE IF NOT EXISTS telegram_schema_migrations (
      version int PRIMARY KEY,
      applied_at timestamptz NOT NULL DEFAULT now()
    )
  `);
  const { rows } = await client.query<{ version: number }>(
    "SELECT version FROM telegram_schema_migrations",
  );
  const applied = new Set(rows.map((row) => Number(row.version)));

  let appliedCount = 0;
  for (const [index, sql] of MIGRATIONS.entries()) {
    const version = index + 1;
    if (applied.has(version)) continue;
    // Each migration commits atomically with its version row so a crash
    // mid-run can never record a version whose DDL did not land (or vice
    // versa).
    await client.query("BEGIN");
    try {
      await client.query(sql);
      await client.query(
        "INSERT INTO telegram_schema_migrations (version) VALUES ($1)",
        [version],
      );
      await client.query("COMMIT");
    } catch (error) {
      await client.query("ROLLBACK");
      throw error;
    }
    appliedCount += 1;
    log.info("telegrambot_migration_applied", { version });
  }

  log.info("telegrambot_migrations_complete", {
    applied_count: appliedCount,
    schema_version: SUPPORTED_SCHEMA_VERSION,
  });
}

/**
 * Readiness gate: fails on pending (db < supported — migrations have not run
 * or did not finish) and on unsupported-future (db > supported — a newer
 * release migrated this database and this build cannot prove compatibility).
 */
export async function checkSchemaVersion(
  pool: Pool,
): Promise<SchemaVersionCheck> {
  let dbVersion = 0;
  try {
    const { rows } = await pool.query<{ version: number | null }>(
      "SELECT max(version) AS version FROM telegram_schema_migrations",
    );
    dbVersion = Number(rows[0]?.version ?? 0);
  } catch (error) {
    // undefined_table: clean database, migrations never ran => pending.
    if (!isUndefinedTable(error)) throw error;
  }

  const state: SchemaVersionState =
    dbVersion === SUPPORTED_SCHEMA_VERSION
      ? "ok"
      : dbVersion < SUPPORTED_SCHEMA_VERSION
        ? "pending"
        : "unsupported_future";
  return {
    ok: state === "ok",
    state,
    dbVersion,
    supportedVersion: SUPPORTED_SCHEMA_VERSION,
  };
}

function isUndefinedTable(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    (error as { code?: unknown }).code === "42P01"
  );
}
