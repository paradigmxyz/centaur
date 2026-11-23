import { describe, expect, it } from "vitest";

import { defaultValues, toPayload } from "./CredentialForm";

describe("toPayload", () => {
  it("sends the refresh policy as JSON numbers", () => {
    const values = { ...defaultValues(), name: "Example", token_endpoint: "https://idp.example.test/token" };
    const payload = toPayload(values);
    expect(payload.early_refresh_fraction).toBe(0.2);
    expect(payload.early_refresh_slack_seconds).toBe(300);
    expect(payload.max_refresh_interval_seconds).toBe(86400);
    expect(payload.refresh_timeout_seconds).toBe(30);
  });

  it("omits blank refresh fields and passes non-numbers through for server validation", () => {
    const values = {
      ...defaultValues(),
      name: "Example",
      token_endpoint: "https://idp.example.test/token",
      early_refresh_fraction: " ",
      refresh_timeout_seconds: "soon",
    };
    const body = JSON.parse(JSON.stringify(toPayload(values)));
    expect(body).not.toHaveProperty("early_refresh_fraction");
    expect(body.refresh_timeout_seconds).toBe("soon");
  });
});
