import { afterEach, describe, expect, it, vi } from "vitest";

import { api, ApiError, loginUrl, navigation, setCsrfToken } from "@/lib/api";
import { apiResponse, mockApi } from "@/test/utils";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  setCsrfToken(null);
});

describe("api client", () => {
  it("sends the CSRF token on writes but not reads", async () => {
    setCsrfToken("tok");
    const mock = mockApi({ "GET /api/v1/console/things": { data: [] }, "POST /api/v1/console/things": { data: { id: "1" } } });
    await api.get("/api/v1/console/things", { q: "a", ids: ["x", "y"], empty: "" });
    await api.post("/api/v1/console/things", { data: { a: 1 } });

    const [get, post] = mock.calls();
    expect(get?.url.search).toBe("?q=a&ids%5B%5D=x&ids%5B%5D=y");
    expect(get?.headers["X-CSRF-Token"]).toBeUndefined();
    expect(post?.headers["X-CSRF-Token"]).toBe("tok");
    expect(post?.body).toEqual({ data: { a: 1 } });
  });

  it("refuses any path outside the console API without a request", async () => {
    const mock = mockApi({});
    for (const path of ["/api/v1/principals", "/api/v1/me", "/api/v1/consolex", "/api/v1/sandbox/permissions", "/up"]) {
      await expect(api.get(path)).rejects.toThrow(/only calls \/api\/v1\/console\//);
      await expect(api.post(path, { data: {} })).rejects.toThrow(/refused/);
    }
    expect(mock.calls()).toEqual([]);
  });

  it("parses the error envelope", async () => {
    mockApi({
      "POST /api/v1/console/things": apiResponse(422, {
        error: { message: "validation failed", details: { foreign_id: ["has already been taken"] } },
      }),
    });
    const error = await api.post("/api/v1/console/things").catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).status).toBe(422);
    expect((error as ApiError).fieldErrors).toEqual({ foreign_id: ["has already been taken"] });
  });

  it("redirects to login on 401 and to pending for pending accounts", async () => {
    const assign = vi.spyOn(navigation, "assign").mockImplementation(() => {});
    mockApi({
      "GET /api/v1/console/a": apiResponse(401, { error: { message: "invalid or missing API key" } }),
      "GET /api/v1/console/b": apiResponse(403, { error: { message: "pending", details: { reason: "account_pending" } } }),
    });
    window.history.replaceState(null, "", "/console/credentials?kind=oauth");
    await expect(api.get("/api/v1/console/a")).rejects.toBeInstanceOf(ApiError);
    // A full page load of the server-rendered login page, back here afterwards.
    expect(assign).toHaveBeenLastCalledWith("/login?return_to=%2Fconsole%2Fcredentials%3Fkind%3Doauth");
    await expect(api.get("/api/v1/console/b")).rejects.toBeInstanceOf(ApiError);
    expect(assign).toHaveBeenLastCalledWith("/pending");
  });

  it("always passes the current page as return_to; the server allowlists it", () => {
    expect(loginUrl({ pathname: "/", search: "" })).toBe("/login?return_to=%2F");
    expect(loginUrl({ pathname: "/console/principals/prn_1", search: "?tab=grants" })).toBe(
      "/login?return_to=%2Fconsole%2Fprincipals%2Fprn_1%3Ftab%3Dgrants",
    );
    expect(loginUrl({ pathname: "/elsewhere", search: "" })).toBe("/login?return_to=%2Felsewhere");
  });

  it("refreshes a stale CSRF token and retries once", async () => {
    setCsrfToken("stale");
    let attempts = 0;
    const mock = mockApi({
      "GET /api/v1/console/me": { data: { csrf_token: "fresh" } },
      "DELETE /api/v1/console/things/1": () => {
        attempts += 1;
        return attempts === 1 ? apiResponse(403, { error: { message: "csrf", details: { reason: "csrf" } } }) : undefined;
      },
    });
    await api.delete("/api/v1/console/things/1");
    const deletes = mock.calls("DELETE /api/v1/console/things/1");
    expect(deletes.map((call) => call.headers["X-CSRF-Token"])).toEqual(["stale", "fresh"]);
  });
});
