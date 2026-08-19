import { describe, expect, it } from "bun:test";
import {
  parseIssueAssignmentWebhook,
  parseIssueCommentWebhook,
} from "../src/issue-comments";

const BOT_USER_ID = "bot-1";

function assignmentPayload(
  topLevel: Record<string, unknown> = {},
  dataOverrides: Record<string, unknown> = {},
): string {
  return JSON.stringify({
    action: "update",
    type: "Issue",
    organizationId: "org-1",
    data: {
      id: "issue-1",
      assigneeId: BOT_USER_ID,
      updatedAt: "2026-06-17T00:00:00.000Z",
      ...dataOverrides,
    },
    ...topLevel,
  });
}

function commentPayload(overrides: Record<string, unknown> = {}): string {
  return JSON.stringify({
    action: "create",
    type: "Comment",
    organizationId: "org-1",
    url: "https://linear.app/acme/comment/comment-9",
    data: {
      id: "comment-9",
      body: "actually, hold off on this",
      issueId: "issue-1",
      createdAt: "2026-06-12T00:00:00.000Z",
      updatedAt: "2026-06-12T00:00:00.000Z",
      user: {
        id: "user-1",
        name: "Ada Lovelace",
        email: "ada@example.com",
        url: "https://linear.app/acme/profiles/ada",
      },
      ...overrides,
    },
  });
}

describe("parseIssueCommentWebhook", () => {
  it("parses a user comment-created webhook", () => {
    expect(parseIssueCommentWebhook(commentPayload())).toEqual({
      authorId: "user-1",
      authorName: "Ada Lovelace",
      body: "actually, hold off on this",
      commentId: "comment-9",
      createdAt: "2026-06-12T00:00:00.000Z",
      issueId: "issue-1",
      parentId: undefined,
      url: "https://linear.app/acme/comment/comment-9",
    });
  });

  it("keeps the parent id for thread replies", () => {
    expect(
      parseIssueCommentWebhook(commentPayload({ parentId: "comment-root" }))
        ?.parentId,
    ).toBe("comment-root");
  });

  it("rejects non-comment, non-create, and malformed payloads", () => {
    expect(
      parseIssueCommentWebhook(
        JSON.stringify({ action: "create", type: "AgentSessionEvent" }),
      ),
    ).toBeNull();
    expect(
      parseIssueCommentWebhook(
        JSON.stringify({ action: "update", type: "Comment", data: {} }),
      ),
    ).toBeNull();
    expect(parseIssueCommentWebhook("not json")).toBeNull();
  });

  it("rejects bot/agent comments (botActor, no user) and empty bodies", () => {
    expect(
      parseIssueCommentWebhook(commentPayload({ user: undefined })),
    ).toBeNull();
    expect(parseIssueCommentWebhook(commentPayload({ body: "  " }))).toBeNull();
    expect(
      parseIssueCommentWebhook(commentPayload({ issueId: undefined })),
    ).toBeNull();
  });
});

describe("parseIssueAssignmentWebhook", () => {
  it("fires when the assignee just changed to the bot", () => {
    const event = parseIssueAssignmentWebhook(
      assignmentPayload({ updatedFrom: { assigneeId: "user-9" } }),
      BOT_USER_ID,
    );
    expect(event?.issueId).toBe("issue-1");
    expect(event?.delegated).toBe(false);
  });

  it("fires when assigned from unassigned (null) to the bot", () => {
    expect(
      parseIssueAssignmentWebhook(
        assignmentPayload({ updatedFrom: { assigneeId: null } }),
        BOT_USER_ID,
      ),
    ).not.toBeNull();
  });

  it("fires when the issue is just delegated to the bot", () => {
    const event = parseIssueAssignmentWebhook(
      assignmentPayload(
        { updatedFrom: { delegateId: "user-9" } },
        { assigneeId: null, delegateId: BOT_USER_ID },
      ),
      BOT_USER_ID,
    );
    expect(event?.issueId).toBe("issue-1");
    expect(event?.delegated).toBe(true);
  });

  it("fires when an issue is CREATED already assigned to the bot (no updatedFrom)", () => {
    expect(
      parseIssueAssignmentWebhook(
        assignmentPayload({ action: "create" }),
        BOT_USER_ID,
      ),
    ).not.toBeNull();
  });

  it("fires when an issue is CREATED already delegated to the bot", () => {
    const event = parseIssueAssignmentWebhook(
      assignmentPayload(
        { action: "create" },
        { assigneeId: null, delegateId: BOT_USER_ID },
      ),
      BOT_USER_ID,
    );
    expect(event?.delegated).toBe(true);
  });

  it("does NOT fire on an edit to an issue the bot already owns", () => {
    expect(
      parseIssueAssignmentWebhook(
        assignmentPayload({ updatedFrom: { description: "old text" } }),
        BOT_USER_ID,
      ),
    ).toBeNull();
  });

  it("does NOT fire on the bot's own status change (updatedFrom lacks assignee/delegate)", () => {
    expect(
      parseIssueAssignmentWebhook(
        assignmentPayload({ updatedFrom: { stateId: "st-old" } }),
        BOT_USER_ID,
      ),
    ).toBeNull();
  });

  it("falls back to the membership check when updatedFrom is absent", () => {
    expect(
      parseIssueAssignmentWebhook(assignmentPayload(), BOT_USER_ID),
    ).not.toBeNull();
  });

  it("ignores updates where neither assignee nor delegate is the bot", () => {
    expect(
      parseIssueAssignmentWebhook(
        assignmentPayload(
          { updatedFrom: { assigneeId: BOT_USER_ID } },
          { assigneeId: "user-9" },
        ),
        BOT_USER_ID,
      ),
    ).toBeNull();
  });

  it("does NOT fire when the bot assigned the issue to itself", () => {
    expect(
      parseIssueAssignmentWebhook(
        assignmentPayload({
          actor: { id: BOT_USER_ID, name: "centaur", type: "user" },
          updatedFrom: { assigneeId: null },
        }),
        BOT_USER_ID,
      ),
    ).toBeNull();
  });

  it("does NOT fire when the bot delegated the issue to itself", () => {
    expect(
      parseIssueAssignmentWebhook(
        assignmentPayload(
          {
            actor: { id: BOT_USER_ID, name: "centaur", type: "user" },
            updatedFrom: { delegateId: null },
          },
          { assigneeId: null, delegateId: BOT_USER_ID },
        ),
        BOT_USER_ID,
      ),
    ).toBeNull();
  });

  it("does NOT fire when the bot creates an issue pre-assigned to itself", () => {
    expect(
      parseIssueAssignmentWebhook(
        assignmentPayload({
          action: "create",
          actor: { id: BOT_USER_ID, name: "centaur", type: "user" },
        }),
        BOT_USER_ID,
      ),
    ).toBeNull();
  });

  it("still fires when someone ELSE'S actor hands the issue to the bot", () => {
    expect(
      parseIssueAssignmentWebhook(
        assignmentPayload({
          actor: { id: "user-9", name: "Ada Lovelace", type: "user" },
          updatedFrom: { assigneeId: null },
        }),
        BOT_USER_ID,
      ),
    ).not.toBeNull();
  });
});
