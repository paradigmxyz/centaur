import { describe, expect, test } from "bun:test";
import {
  DEFAULT_CLIENT_CREDENTIAL_SCOPES,
  linearAdapterAuth,
} from "../src/linear-auth";

describe("linearAdapterAuth", () => {
  test("client credentials win over a static token and API key", () => {
    expect(
      linearAdapterAuth({
        linearAccessToken: "token",
        linearApiKey: "key",
        linearClientCredentials: { clientId: "id", clientSecret: "secret" },
      }),
    ).toEqual({
      clientCredentials: {
        clientId: "id",
        clientSecret: "secret",
        scopes: DEFAULT_CLIENT_CREDENTIAL_SCOPES,
      },
    });
  });

  test("default scopes include app:assignable for delegation", () => {
    expect(DEFAULT_CLIENT_CREDENTIAL_SCOPES).toContain("app:assignable");
    expect(DEFAULT_CLIENT_CREDENTIAL_SCOPES).toContain("app:mentionable");
  });

  test("explicit scopes replace the defaults; empty falls back", () => {
    const auth = (scopes: string[]) =>
      linearAdapterAuth({
        linearClientCredentials: { clientId: "id", clientSecret: "s", scopes },
      });
    expect(auth(["read"])).toEqual({
      clientCredentials: {
        clientId: "id",
        clientSecret: "s",
        scopes: ["read"],
      },
    });
    expect(auth([])).toEqual({
      clientCredentials: {
        clientId: "id",
        clientSecret: "s",
        scopes: DEFAULT_CLIENT_CREDENTIAL_SCOPES,
      },
    });
  });

  test("access token wins over API key, then API key, then nothing", () => {
    expect(
      linearAdapterAuth({ linearAccessToken: "token", linearApiKey: "key" }),
    ).toEqual({ accessToken: "token" });
    expect(linearAdapterAuth({ linearApiKey: "key" })).toEqual({
      apiKey: "key",
    });
    expect(linearAdapterAuth({})).toEqual({});
  });
});
