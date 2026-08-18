import { describe, expect, it } from "bun:test";
import {
  buildCommentReplyBody,
  buildThinkingReplyBody,
  CommentReplyCollector,
} from "../src/comment-bot";
import type { ChatSDKStreamChunk } from "@centaur/rendering";

function command(id: string, details: string): ChatSDKStreamChunk {
  return {
    type: "task_update",
    id,
    title: "Command execution",
    status: "complete",
    details,
  };
}

function thinking(id: string, details: string): ChatSDKStreamChunk {
  return {
    type: "task_update",
    id,
    title: "Thinking",
    status: "complete",
    details,
  };
}

describe("CommentReplyCollector chain-of-thought flattening", () => {
  it("renders a fenced command as an inline code span, not a list-breaking fence", () => {
    const collector = new CommentReplyCollector();
    collector.update(
      command("cmd-1", "```sh\nsed -n '200,380p' src/services/options.ts\n```"),
    );
    expect(collector.cotLines).toEqual([
      "Command execution: `sed -n '200,380p' src/services/options.ts`",
    ]);
    // The whole point: no raw triple-backtick fence survives into the bullet.
    expect(collector.cotLines.join("\n")).not.toContain("```");
  });

  it("collapses a multi-line command and strips inner backticks", () => {
    const collector = new CommentReplyCollector();
    collector.update(command("cmd-1", "```sh\necho `date`\nls -la\n```"));
    expect(collector.cotLines).toEqual([
      "Command execution: `echo 'date' ls -la`",
    ]);
  });

  it("records in-progress task details for live replies without duplicating the terminal update", () => {
    const collector = new CommentReplyCollector();
    collector.update({
      type: "task_update",
      id: "cmd-1",
      title: "1. Command execution",
      status: "in_progress",
      details: "```sh\npnpm test\n```",
    });
    collector.update({
      type: "task_update",
      id: "cmd-1",
      title: "1. Command execution",
      status: "complete",
    });

    expect(collector.cotLines).toEqual(["1. Command execution: `pnpm test`"]);
  });

  it("tracks the latest reasoning as the current thought", () => {
    const collector = new CommentReplyCollector();
    collector.update(thinking("t-1", "First, read the options."));
    collector.update(command("cmd-1", "```sh\nls\n```"));
    collector.update(thinking("t-2", "Now check the guard covers every path."));
    expect(collector.latestThought).toBe(
      "Now check the guard covers every path.",
    );
  });
});

describe("buildThinkingReplyBody", () => {
  it("puts the current thought in the body, above the collapsed section", () => {
    const body = buildThinkingReplyBody(
      ["Command execution: `ls`"],
      "Inspecting the backend.",
    );
    expect(body.startsWith("Inspecting the backend.\n\n")).toBe(true);
    expect(body).toContain(">>> Thinking…");
    expect(body).toContain("- Command execution: `ls`");
    // The thought precedes the fold.
    expect(body.indexOf("Inspecting the backend.")).toBeLessThan(
      body.indexOf(">>> Thinking…"),
    );
  });

  it("omits the headline when there's no thought yet", () => {
    const body = buildThinkingReplyBody(["Command execution: `ls`"]);
    expect(body.startsWith(">>> Thinking…")).toBe(true);
  });
});

describe("buildCommentReplyBody", () => {
  it("leads with the answer and folds the chain of thought", () => {
    const body = buildCommentReplyBody({
      answer: "About a day.",
      cotLines: ["Command execution: `ls`"],
    });
    expect(body.startsWith("About a day.")).toBe(true);
    expect(body).toContain(">>> Chain of thought");
  });
});
