import { afterEach, describe, expect, it, vi } from "vitest";

import { api, ApiError, navigation, setCsrfToken } from "@/lib/api";
import { apiResponse, mockApi } from "@/test/utils";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  setCsrfToken(null);
});

describe("api client", () => {
  it("sends the CSRF token on writes but not reads", async () => {
    setCsrfToken("tok");
    const mock = mockApi({ "GET /api/v1/things": { data: [] }, "POST /api/v1/things": { data: { id: "1" } } });
    await api.get("/api/v1/things", { q: "a", ids: ["x", "y"], empty: "" });
    await api.post("/api/v1/things", { data: { a: 1 } });

    const [get, post] = mock.calls();
    expect(get?.url.search).toBe("?q=a&ids%5B%5D=x&ids%5B%5D=y");
    expect(get?.headers["X-CSRF-Token"]).toBeUndefined();
    expect(post?.headers["X-CSRF-Token"]).toBe("tok");
    expect(post?.body).toEqual({ data: { a: 1 } });
  });

  it("parses the error envelope", async () => {
    mockApi({
      "POST /api/v1/things": apiResponse(422, {
        error: { message: "validation failed", details: { foreign_id: ["has already been taken"] } },
      }),
    });
    const error = await api.post("/api/v1/things").catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).status).toBe(422);
    expect((error as ApiError).fieldErrors).toEqual({ foreign_id: ["has already been taken"] });
  });

  it("redirects to login on 401 and to pending for pending accounts", async () => {
    const assign = vi.spyOn(navigation, "assign").mockImplementation(() => {});
    mockApi({
      "GET /api/v1/a": apiResponse(401, { error: { message: "invalid or missing API key" } }),
      "GET /api/v1/b": apiResponse(403, { error: { message: "pending", details: { reason: "account_pending" } } }),
    });
    await expect(api.get("/api/v1/a")).rejects.toBeInstanceOf(ApiError);
    expect(assign).toHaveBeenLastCalledWith(expect.stringMatching(/^\/login/));
    await expect(api.get("/api/v1/b")).rejects.toBeInstanceOf(ApiError);
    expect(assign).toHaveBeenLastCalledWith("/pending");
  });

  it("refreshes a stale CSRF token and retries once", async () => {
    setCsrfToken("stale");
    let attempts = 0;
    const mock = mockApi({
      "GET /api/v1/me": { data: { csrf_token: "fresh" } },
      "DELETE /api/v1/things/1": () => {
        attempts += 1;
        return attempts === 1 ? apiResponse(403, { error: { message: "csrf", details: { reason: "csrf" } } }) : undefined;
      },
    });
    await api.delete("/api/v1/things/1");
    const deletes = mock.calls("DELETE /api/v1/things/1");
    expect(deletes.map((call) => call.headers["X-CSRF-Token"])).toEqual(["stale", "fresh"]);
  });
});
