import { describe, expect, it } from "vitest";

import type { KindRecord } from "./api";
import { errorMessages, rulesPayload, settingsPayload, sourcePayload, splitTerms, toFormValues, toPayload } from "./form-model";

const base = { id: "ssr_1", foreign_id: null, name: "n", description: null, labels: {}, created_at: "", updated_at: "" };

describe("secret form model", () => {
  it("never prefills an inline value and omits a blank one", () => {
    const record: KindRecord = { ...base, inject_config: { header: "X-Key" }, source: { source_type: "control_plane", config: {} } };
    const values = toFormValues("static", record);
    expect(values.source.secret).toBe("");
    expect(sourcePayload(values.source, record.source ?? null)).toEqual({ source_type: "control_plane", config: {} });
    expect(sourcePayload({ ...values.source, secret: "new" }, record.source ?? null)).toEqual({
      source_type: "control_plane",
      config: {},
      secret: "new",
    });
  });

  it("maps references to the backend's config key and keeps config it does not edit", () => {
    const stored = { source_type: "aws_ssm", config: { name: "/old", with_decryption: true, region: "us-east-1" } };
    const values = { source_type: "aws_ssm", reference: " /new ", secret: "", region: "", json_key: "token" };
    expect(sourcePayload(values, stored)).toEqual({
      source_type: "aws_ssm",
      config: { name: "/new", with_decryption: true, json_key: "token" },
    });
    expect(sourcePayload({ ...values, source_type: "env", reference: "VAR" }, stored)).toEqual({
      source_type: "env",
      config: { var: "VAR", json_key: "token" },
    });
  });

  it("drops blank rule and setting rows and splits terms", () => {
    expect(
      rulesPayload([
        { host: " ", cidr: "", http_methods: [], paths: [] },
        { host: "api.example.com", cidr: "", http_methods: ["get"], paths: ["/v1/*"] },
      ]),
    ).toEqual([{ host: "api.example.com", cidr: null, http_methods: ["GET"], paths: ["/v1/*"] }]);
    expect(
      settingsPayload([
        { name: "app.tenant", kind: "literal", value: "centaur" },
        { name: "", kind: "literal", value: "ignored" },
        { name: "app.who", kind: "principal_field", value: " foreign_id " },
      ]),
    ).toEqual([
      { name: "app.tenant", value: "centaur" },
      { name: "app.who", value_from: { principal_field: "foreign_id" } },
    ]);
    expect(splitTerms("a, b\nc  d")).toEqual(["a", "b", "c", "d"]);
  });

  it("round-trips pg_dsn settings and gcp credential modes", () => {
    const pg = toFormValues("pg_dsn", {
      ...base,
      database: "shop",
      settings: [{ name: "app.who", value_from: { proxy_label: "team" } }],
      dsn: { source_type: "env", config: { var: "DSN" } },
    });
    expect(pg.settings).toEqual([{ name: "app.who", kind: "proxy_label", value: "team" }]);
    expect(pg.source.reference).toBe("DSN");

    const wi = toFormValues("gcp_auth", { ...base, credentials_provider: { type: "workload_identity" }, scopes: ["a", "b"] });
    expect(wi.credential_mode).toBe("workload_identity");
    expect(toPayload("gcp_auth", wi)).toMatchObject({
      credentials_provider: { type: "workload_identity" },
      subject: null,
      scopes: ["a", "b"],
    });
    expect(toPayload("gcp_auth", wi)).not.toHaveProperty("keyfile");

    const idToken = toFormValues("gcp_id_token", { ...base, header: null });
    expect(idToken.id_token_header).toBe("authorization");
    expect(toPayload("gcp_id_token", idToken)).toMatchObject({ header: null });
  });

  it("builds static inject and replace configs", () => {
    const values = toFormValues("static");
    expect(toPayload("static", { ...values, inject_header: "Authorization", formatter: "Bearer {{ .Value }}" })).toMatchObject({
      kind: "custom",
      inject_config: { header: "Authorization", formatter: "Bearer {{ .Value }}" },
      replace_config: null,
    });
    expect(toPayload("static", values)).toMatchObject({ inject_config: null });
    expect(
      toPayload("static", { ...values, mode: "replace", proxy_value: "REPLACE_ME", match_headers: "A, B", require: true }),
    ).toMatchObject({
      inject_config: null,
      replace_config: { proxy_value: "REPLACE_ME", match_headers: ["A", "B"], require: true },
    });
  });

  it("formats error details like the legacy summary", () => {
    expect(
      errorMessages({
        base: ["must define one of inject_config or replace_config"],
        "source.config": ["var is required"],
        "rules[1].cidr": ["is not a valid CIDR"],
        rules: ["is invalid", "must include at least one rule"],
        foreign_id: ["can't be blank"],
      }),
    ).toEqual([
      "must define one of inject_config or replace_config",
      "Source: Config var is required",
      "Rule 2: Cidr is not a valid CIDR",
      "Rules must include at least one rule",
      "Foreign id can't be blank",
    ]);
  });
});
