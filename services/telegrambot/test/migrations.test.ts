import { afterAll, beforeAll, describe, expect, it } from "bun:test";
import pg from "pg";
import type { Pool } from "pg";
import {
  SUPPORTED_SCHEMA_VERSION,
  checkSchemaVersion,
  runMigrations,
} from "../src/migrations";

/**
 * Migration runner + schema version gate tests against real Postgres.
 * Skipped cleanly when TELEGRAMBOT_TEST_DATABASE_URL is unset (CI has no
 * Postgres). The tests in this file are ordered: they walk a database from
 * clean, through migrated, through simulated version-skew states, and leave
 * the schema fully migrated for the other DB-backed test files.
 */

const databaseUrl = process.env.TELEGRAMBOT_TEST_DATABASE_URL;

const TELEGRAM_TABLES = [
  "telegram_update_inbox",
  "telegram_poll_state",
  "telegram_schema_migrations",
] as const;

describe.skipIf(!databaseUrl)("migrations (requires Postgres)", () => {
  const pool: Pool = new pg.Pool({
    connectionString: databaseUrl,
    max: 10,
    connectionTimeoutMillis: 10_000,
  });

  async function dropTelegramTables(): Promise<void> {
    await pool.query(`DROP TABLE IF EXISTS ${TELEGRAM_TABLES.join(", ")}`);
  }

  async function tableExists(name: string): Promise<boolean> {
    const result = await pool.query<{ oid: string | null }>(
      "SELECT to_regclass($1) AS oid",
      [name],
    );
    return result.rows[0]?.oid != null;
  }

  async function appliedVersions(): Promise<number[]> {
    const result = await pool.query<{ version: number }>(
      "SELECT version FROM telegram_schema_migrations ORDER BY version",
    );
    return result.rows.map((row) => Number(row.version));
  }

  const allVersions = Array.from(
    { length: SUPPORTED_SCHEMA_VERSION },
    (_, index) => index + 1,
  );

  beforeAll(async () => {
    // Hermetic per-run state: this file exercises clean-database migration,
    // so it starts from no telegram_* tables at all.
    await dropTelegramTables();
  });

  afterAll(async () => {
    await pool.end();
  });

  it("checkSchemaVersion reports pending on a clean database", async () => {
    const check = await checkSchemaVersion(pool);
    expect(check).toEqual({
      ok: false,
      state: "pending",
      dbVersion: 0,
      supportedVersion: SUPPORTED_SCHEMA_VERSION,
    });
  });

  it("migrates a clean database and passes checkSchemaVersion at SUPPORTED_SCHEMA_VERSION", async () => {
    await runMigrations(pool);

    expect(await tableExists("telegram_poll_state")).toBe(true);
    expect(await tableExists("telegram_update_inbox")).toBe(true);
    expect(await tableExists("telegram_schema_migrations")).toBe(true);
    expect(await appliedVersions()).toEqual(allVersions);

    const check = await checkSchemaVersion(pool);
    expect(check).toEqual({
      ok: true,
      state: "ok",
      dbVersion: SUPPORTED_SCHEMA_VERSION,
      supportedVersion: SUPPORTED_SCHEMA_VERSION,
    });
  });

  it("re-running migrations is idempotent and preserves existing data", async () => {
    await pool.query(
      `INSERT INTO telegram_poll_state (bot_user_id, receive_offset)
       VALUES ('migration-idempotency-bot', 42)
       ON CONFLICT (bot_user_id) DO UPDATE SET receive_offset = 42`,
    );

    await runMigrations(pool);
    await runMigrations(pool);

    expect(await appliedVersions()).toEqual(allVersions);
    const row = await pool.query<{ receive_offset: string }>(
      "SELECT receive_offset FROM telegram_poll_state WHERE bot_user_id = 'migration-idempotency-bot'",
    );
    expect(Number(row.rows[0]?.receive_offset)).toBe(42);
    expect((await checkSchemaVersion(pool)).ok).toBe(true);
  });

  it("checkSchemaVersion fails pending when the version ledger is emptied, and a re-run repairs it additively", async () => {
    await pool.query("TRUNCATE telegram_schema_migrations");

    const pending = await checkSchemaVersion(pool);
    expect(pending.ok).toBe(false);
    expect(pending.state).toBe("pending");
    expect(pending.dbVersion).toBe(0);

    // Re-run repairs the ledger without recreating (and thus wiping) the
    // tables: the data planted by the previous test must survive.
    await runMigrations(pool);
    expect((await checkSchemaVersion(pool)).ok).toBe(true);
    const row = await pool.query<{ receive_offset: string }>(
      "SELECT receive_offset FROM telegram_poll_state WHERE bot_user_id = 'migration-idempotency-bot'",
    );
    expect(Number(row.rows[0]?.receive_offset)).toBe(42);
  });

  it("checkSchemaVersion fails closed on an unsupported future schema version", async () => {
    const futureVersion = SUPPORTED_SCHEMA_VERSION + 1;
    await pool.query(
      "INSERT INTO telegram_schema_migrations (version) VALUES ($1)",
      [futureVersion],
    );

    const check = await checkSchemaVersion(pool);
    expect(check.ok).toBe(false);
    expect(check.state).toBe("unsupported_future");
    expect(check.dbVersion).toBe(futureVersion);

    // Restore a supported schema so later files see a clean migrated state.
    await pool.query(
      "DELETE FROM telegram_schema_migrations WHERE version = $1",
      [futureVersion],
    );
    expect((await checkSchemaVersion(pool)).ok).toBe(true);
  });

  it("concurrent runMigrations calls on a clean database serialize safely", async () => {
    await dropTelegramTables();

    await Promise.all([
      runMigrations(pool),
      runMigrations(pool),
      runMigrations(pool),
    ]);

    // Exactly one ledger row per version — the advisory lock kept the three
    // runners from double-applying or racing the ledger CREATE.
    expect(await appliedVersions()).toEqual(allVersions);
    expect((await checkSchemaVersion(pool)).ok).toBe(true);

    // The migrated schema is actually usable afterwards.
    await pool.query(
      "INSERT INTO telegram_poll_state (bot_user_id) VALUES ('migration-concurrency-bot')",
    );
    const count = await pool.query<{ count: number }>(
      "SELECT count(*)::int AS count FROM telegram_poll_state WHERE bot_user_id = 'migration-concurrency-bot'",
    );
    expect(count.rows[0]?.count).toBe(1);
  });
});
