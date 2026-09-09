import { describe, expect, test } from "bun:test";
import { resolveGithubbotAuth } from "../src/auth";

describe("resolveGithubbotAuth", () => {
  test("accepts a personal access token", () => {
    expect(resolveGithubbotAuth({ token: " token " })).toEqual({ token: "token" });
  });

  test("accepts complete GitHub App credentials", () => {
    expect(
      resolveGithubbotAuth({
        appId: "123",
        installationId: 456,
        privateKey: "private-key",
      }),
    ).toEqual({ appId: "123", installationId: 456, privateKey: "private-key" });
  });

  test("rejects mixed authentication modes", () => {
    expect(() =>
      resolveGithubbotAuth({
        appId: "123",
        installationId: 456,
        privateKey: "private-key",
        token: "token",
      }),
    ).toThrow("not both");
  });

  test("rejects incomplete GitHub App credentials without leaking values", () => {
    const privateKey = "sensitive-private-key";
    expect(() => resolveGithubbotAuth({ appId: "123", privateKey })).toThrow(
      "all of GITHUB_APP_ID",
    );
    try {
      resolveGithubbotAuth({ appId: "123", privateKey });
    } catch (error) {
      expect(String(error)).not.toContain(privateKey);
    }
  });

  test("rejects missing authentication", () => {
    expect(() => resolveGithubbotAuth({})).toThrow("GitHub authentication requires");
  });

  test("rejects an unsafe installation ID", () => {
    expect(() =>
      resolveGithubbotAuth({
        appId: "123",
        installationId: Number.MAX_SAFE_INTEGER + 1,
        privateKey: "private-key",
      }),
    ).toThrow("positive integer");
  });

  test("rejects a zero installation ID", () => {
    expect(() =>
      resolveGithubbotAuth({
        appId: "123",
        installationId: 0,
        privateKey: "private-key",
      }),
    ).toThrow("positive integer");
  });
});
