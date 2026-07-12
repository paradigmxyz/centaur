import pg from "pg";
import type { Pool, PoolClient } from "pg";
import type { Logger } from "./types";
import { errorMessage, noopLogger } from "./utils";

/**
 * Telegram delta: discordbot hands its pool to the Chat SDK Postgres state
 * adapter; telegrambot owns its tables directly (see src/migrations.ts), so
 * the pool and the explicit-transaction helper live here instead.
 *
 * The 'error' listener exists for the same reason as discordbot's: pg.Pool
 * emits 'error' for idle clients whose connection drops (Postgres restart, a
 * network blip during pod startup). Without a listener node-postgres rethrows
 * it as an uncaught exception; logging and swallowing lets the pool reconnect
 * on the next query.
 */
export function createPool(postgresUrl: string, logger?: Logger): Pool {
  const log = logger ?? noopLogger;
  const pool = new pg.Pool({
    connectionString: postgresUrl,
    max: 10,
    idleTimeoutMillis: 30_000,
    connectionTimeoutMillis: 10_000,
  });
  pool.on("error", (error) => {
    log.warn("telegrambot_postgres_pool_error", {
      error: errorMessage(error),
    });
  });
  return pool;
}

/**
 * Runs `fn` on a checked-out client between BEGIN/COMMIT, rolling back on
 * throw. The receipt contract (spec §2a) requires inbox upsert and cursor
 * advancement to commit or fail together on ONE connection — independent
 * pool.query calls each grab their own client and cannot provide that.
 *
 * If ROLLBACK itself fails the connection state is unknown, so the client is
 * destroyed (released with an error) rather than returned to the pool where a
 * dangling open transaction could poison later queries.
 */
export async function withTransaction<T>(
  pool: Pool,
  fn: (client: PoolClient) => Promise<T>,
): Promise<T> {
  const client = await pool.connect();
  let destroy: Error | undefined;
  try {
    await client.query("BEGIN");
    try {
      const result = await fn(client);
      await client.query("COMMIT");
      return result;
    } catch (error) {
      try {
        await client.query("ROLLBACK");
      } catch (rollbackError) {
        destroy =
          rollbackError instanceof Error
            ? rollbackError
            : new Error(errorMessage(rollbackError));
      }
      throw error;
    }
  } finally {
    client.release(destroy);
  }
}
