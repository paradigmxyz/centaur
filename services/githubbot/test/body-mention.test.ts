import { describe, expect, test } from "bun:test";
import { handleBodyMention, mentionsBot } from "../src/body-mention";
import type { PrManagerContext } from "../src/pr-manager";
import { FAKE_TURN_COMMAND, fakeSessionApiFetch } from "./fake-session-api";

describe("mentionsBot", () => {
  test("matches a standalone @-mention case-insensitively", () => {
    expect(mentionsBot("hey @centaur-bot please look", "centaur-bot")).toBe(true);
    expect(mentionsBot("@Centaur-Bot at the start", "centaur-bot")).toBe(true);
    expect(mentionsBot("ping\n@centaur-bot", "centaur-bot")).toBe(true);
  });

  test("does not match substrings, emails, or absence", () => {
    expect(mentionsBot("see @centaur-bot-helper", "centaur-bot")).toBe(false);
    expect(mentionsBot("mail me@centaur-bot.com", "centaur-bot")).toBe(false);
    expect(mentionsBot("no mention here", "centaur-bot")).toBe(false);
  });
});

type Spies = { reactions: number; comments: number; bodies?: string[] };

function makeCtx(
  spies: Spies,
  // Non-retryable so the backgrounded turn settles instantly off the network.
  fetchFn: typeof fetch = (() =>
    Promise.resolve(new Response("no", { status: 400 }))) as unknown as typeof fetch,
): PrManagerContext {
  const m = new Map<string, unknown>();
  const state = {
    get: async (k: string) => m.get(k),
    set: async (k: string, v: unknown) => {
      m.set(k, v);
    },
    setIfNotExists: async (k: string, v: unknown) => {
      if (m.has(k)) return false;
      m.set(k, v);
      return true;
    },
  };
  return {
    octokit: {
      rest: {
        reactions: {
          createForIssue: async () => {
            spies.reactions += 1;
            return { data: { id: 1 } };
          },
        },
        issues: {
          createComment: async (params: { body: string }) => {
            spies.comments += 1;
            spies.bodies?.push(params.body);
            return { data: { id: 2 } };
          },
        },
      },
    },
    options: {
      apiUrl: "http://127.0.0.1:8080",
      stateKeyPrefix: "test",
      logger: {
        debug() {},
        info() {},
        warn() {},
        error() {},
        child() {
          return this;
        },
      },
      fetch: fetchFn,
    },
    state,
    userName: "centaur-bot",
  } as unknown as PrManagerContext;
}

function openedPr(
  body: string,
  association = "MEMBER",
  author = "someone",
  isPrivate?: boolean,
) {
  return JSON.stringify({
    action: "opened",
    repository: {
      full_name: "0xSplits/centaur",
      ...(isPrivate === undefined ? {} : { private: isPrivate }),
    },
    pull_request: {
      number: 7,
      body,
      author_association: association,
      user: { login: author },
    },
  });
}

describe("handleBodyMention", () => {
  test("ignores non issue/PR events", () => {
    expect(
      handleBodyMention(makeCtx({ reactions: 0, comments: 0 }), "push", "{}"),
    ).toBeNull();
  });

  test("ignores actions other than opened", () => {
    const body = JSON.stringify({
      action: "edited",
      repository: { full_name: "o/r" },
      pull_request: { number: 1, body: "@centaur-bot" },
    });
    expect(
      handleBodyMention(
        makeCtx({ reactions: 0, comments: 0 }),
        "pull_request",
        body,
      ),
    ).toBeNull();
  });

  test("ignores bodies without a bot mention", () => {
    expect(
      handleBodyMention(
        makeCtx({ reactions: 0, comments: 0 }),
        "pull_request",
        openedPr("nothing here"),
      ),
    ).toBeNull();
  });

  test("ignores the bot's own issue/PR", () => {
    expect(
      handleBodyMention(
        makeCtx({ reactions: 0, comments: 0 }),
        "pull_request",
        openedPr("@centaur-bot do it", "MEMBER", "centaur-bot"),
      ),
    ).toBeNull();
  });

  test("ignores an unauthorized author", () => {
    const spies = { reactions: 0, comments: 0 };
    expect(
      handleBodyMention(
        makeCtx(spies),
        "pull_request",
        openedPr("@centaur-bot do it", "NONE"),
      ),
    ).toBeNull();
    expect(spies.comments).toBe(0);
  });

  test("runs a turn and replies once for an authorized mention; dedups redelivery", async () => {
    const spies = { reactions: 0, comments: 0 };
    const ctx = makeCtx(spies);
    const first = handleBodyMention(
      ctx,
      "pull_request",
      openedPr("@centaur-bot please review"),
    );
    expect(first).not.toBeNull();
    await first;
    // A redelivery of the same opened webhook must not reply again.
    await handleBodyMention(
      ctx,
      "pull_request",
      openedPr("@centaur-bot please review"),
    );
    expect(spies.comments).toBe(1);
    expect(spies.reactions).toBeGreaterThanOrEqual(1);
  });

  for (const [visibility, isPrivate, shown] of [
    ["a private", true, true],
    ["a public", false, false],
    ["an unspecified", undefined, false],
  ] as const) {
    test(`replies on ${visibility} repository ${shown ? "with" : "without"} the chain of thought`, async () => {
      const spies: Spies = { reactions: 0, comments: 0, bodies: [] };
      await handleBodyMention(
        makeCtx(spies, fakeSessionApiFetch("Done.")),
        "pull_request",
        openedPr("@centaur-bot please review", "MEMBER", "someone", isPrivate),
      );
      expect(spies.bodies).toHaveLength(1);
      const [reply] = spies.bodies ?? [];
      expect(reply).toStartWith("Done.");
      expect(reply?.includes(FAKE_TURN_COMMAND)).toBe(shown);
      expect(reply?.includes("<details>")).toBe(shown);
    });
  }
});
