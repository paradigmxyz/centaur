// Contract test for the company-context staging table: the real sink writes
// into a database migrated with the company-context service's own migrations.
// Needs a ParadeDB superuser URL (the migrations use pg_search and pgvector);
// skipped without one.
import { afterAll, beforeAll, describe, expect, it } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import pg from "pg";
import {
  createPostgresContextSink,
  type TelegramContextSink,
} from "../src/context-sink";

const ADMIN_URL = process.env.TELEGRAMBOT_CONTEXT_TEST_DATABASE_URL?.trim();
const MIGRATIONS = join(import.meta.dir, "../../company-context/migrations");

describe.skipIf(!ADMIN_URL)("company context staging table", () => {
  const database = `telegrambot_context_${process.pid}_${Date.now()}`;
  let url: string;
  let sink: TelegramContextSink;

  beforeAll(async () => {
    const admin = new pg.Client({ connectionString: ADMIN_URL });
    await admin.connect();
    await admin.query(`CREATE DATABASE "${database}"`);
    await admin.end();
    const target = new URL(ADMIN_URL!);
    target.pathname = `/${database}`;
    url = target.toString();
    const client = new pg.Client({ connectionString: url });
    await client.connect();
    // Mirror the service's migration setup.
    await client.query(
      "CREATE SCHEMA IF NOT EXISTS company_context_system; SET search_path TO company_context_system, public",
    );
    const files = readdirSync(MIGRATIONS)
      .filter((name) => name.endsWith(".sql"))
      .sort();
    for (const file of files) {
      await client.query(readFileSync(join(MIGRATIONS, file), "utf8"));
    }
    await client.end();
    sink = createPostgresContextSink(url, () => undefined);
  });

  afterAll(async () => {
    await sink?.close?.();
    if (!ADMIN_URL) return;
    const admin = new pg.Client({ connectionString: ADMIN_URL });
    await admin.connect();
    await admin.query(`DROP DATABASE IF EXISTS "${database}" WITH (FORCE)`);
    await admin.end();
  });

  it("stores a message once, with its UTC projection day", async () => {
    const message = {
      chatId: "-1001234567890",
      chatTitle: "Ops Room",
      media: [
        { kind: "document", mime_type: "application/pdf", name: "plan.pdf" },
      ],
      messageId: 42,
      replyToMessageId: 41,
      senderId: "5551234",
      senderName: "Ada Lovelace",
      senderUsername: "ada",
      // 2023-11-14 23:59:59 UTC.
      sentAt: 1_700_006_399,
      text: "see the plan",
      topicId: 7,
    };
    await sink.record(message);
    await sink.record({ ...message, text: "redelivered" });

    const client = new pg.Client({ connectionString: url });
    await client.connect();
    const { rows } = await client.query(
      `SELECT chat_id::text, message_id::int, topic_id::int, chat_title, sender_id::text,
              sender_name, sender_username, reply_to_message_id::int, text, media,
              sent_at, projection_day::text
       FROM company_context_system.telegram_messages`,
    );
    await client.end();
    expect(rows).toHaveLength(1);
    expect(rows[0]).toEqual({
      chat_id: "-1001234567890",
      message_id: 42,
      topic_id: 7,
      chat_title: "Ops Room",
      sender_id: "5551234",
      sender_name: "Ada Lovelace",
      sender_username: "ada",
      reply_to_message_id: 41,
      text: "see the plan",
      media: [
        { kind: "document", mime_type: "application/pdf", name: "plan.pdf" },
      ],
      sent_at: new Date(1_700_006_399_000),
      projection_day: "2023-11-14",
    });
  });
});
