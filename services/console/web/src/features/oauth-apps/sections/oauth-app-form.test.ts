import { describe, expect, it } from "vitest";

import { initialValues, toPayload } from "./OauthAppForm";

describe("OauthAppForm payload", () => {
  it("sends owner_only and shared only on create", () => {
    const values = { ...initialValues(undefined, "custom"), slug: "personal-mcp", owner_only: true };
    expect(toPayload(values, true, [])).toMatchObject({ owner_only: true, shared: false });
    const update = toPayload(values, false, []);
    expect(update).not.toHaveProperty("owner_only");
    expect(update).not.toHaveProperty("shared");
  });

  it("starts from the stored owner_only value", () => {
    expect(initialValues(undefined, "custom").owner_only).toBe(false);
  });
});
