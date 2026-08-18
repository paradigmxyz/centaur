import { describe, expect, test } from "bun:test";
import { issueWorkThreadKey } from "../src/issue-manager";
import { managementThreadKey } from "../src/pr-manager";
import { reviewThreadKey } from "../src/review";

// api-rs scopes githubbot's API key to a fixed set of thread-key prefixes, so
// every family minted here has to stay listed in its ingress spec
// (services/api-rs/crates/centaur-api-server/src/auth.rs). Renaming one without
// updating that list 403s the whole flow, and only in background paths where
// the failure never reaches a human.
describe("session thread-key families", () => {
  test("issue-work, management and review keys keep their prefixes", () => {
    expect(issueWorkThreadKey("acme", "repo", 7)).toBe(
      "github-issue:acme/repo:7",
    );
    expect(managementThreadKey("acme", "repo", 7)).toBe(
      "github-manage:acme/repo:7",
    );
    expect(reviewThreadKey("acme", "repo", 7)).toBe(
      "github-review:acme/repo:7",
    );
  });
});
