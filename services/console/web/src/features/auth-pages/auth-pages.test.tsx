import { describe, expect, it } from "vitest";

import { isSpaPath } from "@/app/spa-paths";

import authPages from "./index";

describe("auth pages module", () => {
  it("leaves the server-rendered sign-in and OAuth pages to Rails", () => {
    expect(authPages.routes).toBeUndefined();
    expect(authPages.publicRoutes).toBeUndefined();
    for (const path of ["/login", "/pending", "/logout", "/mcp/oauth/authorize", "/oauth/google/start"]) {
      expect(isSpaPath(path)).toBe(false);
    }
  });
});
