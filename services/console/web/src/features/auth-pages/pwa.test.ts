import { describe, expect, it } from "vitest";

import html from "../../../index.html?raw";
import { SERVICE_WORKER_URL } from "./pwa";

describe("PWA registration", () => {
  it("registers the path the console serves the service worker from", () => {
    expect(SERVICE_WORKER_URL).toBe("/service-worker");
  });

  it("links the path the console serves the manifest from", () => {
    expect(html).toContain('<link rel="manifest" href="/manifest" />');
  });
});
