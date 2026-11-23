import { afterEach, describe, expect, it, vi } from "vitest";

import { api, ApiError, loginUrl, navigation } from "@/lib/api";
import { apiResponse, mockApi } from "@/test/utils";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("api client", () => {
  it("encodes query arrays and JSON bodies", async () => {
    const mock = mockApi({ "GET /api/v1/console/things": { data: [] }, "POST /api/v1/console/things": { data: { id: "1" } } });
    await api.get("/api/v1/console/things", { q: "a", ids: ["x", "y"], empty: "" });
    await api.post("/api/v1/console/things", { data: { a: 1 } });

    const [get, post] = mock.calls();
    expect(get?.url.search).toBe("?q=a&ids%5B%5D=x&ids%5B%5D=y");
    expect(post?.body).toEqual({ data: { a: 1 } });
  });

  it("refuses any path outside the console API without a request", async () => {
    const mock = mockApi({});
    for (const path of [
      "/api/v1/principals",
      "/api/v1/me",
      "/api/v1/consolex",
      "/api/v1/sandbox/permissions",
      "/api/v1/console/../principals",
      "/api/v1/console/%2e%2e/principals",
      "/api/v1/console/..\\principals",
      "//evil.example/api/v1/console/me",
    ]) {
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

  it("does not retry a csrf rejection", async () => {
    const mock = mockApi({
      "DELETE /api/v1/console/things/1": apiResponse(403, { error: { message: "csrf", details: { reason: "csrf" } } }),
    });
    await expect(api.delete("/api/v1/console/things/1")).rejects.toBeInstanceOf(ApiError);
    expect(mock.calls("DELETE /api/v1/console/things/1")).toHaveLength(1);
  });
});
