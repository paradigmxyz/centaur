const port = Number(Bun.env.PORT ?? "3002");
const apiUrl = (Bun.env.MONITORING_API_URL ?? "http://centaur-centaur-api:8000").replace(/\/$/, "");
const apiKey = Bun.env.MONITORING_OPERATOR_API_KEY ?? "";
const dist = new URL("../dist/", import.meta.url);

const mimeTypes: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
};

function contentType(pathname: string): string {
  const ext = pathname.slice(pathname.lastIndexOf("."));
  return mimeTypes[ext] ?? "application/octet-stream";
}

async function serveAsset(pathname: string): Promise<Response> {
  if (pathname.includes("..")) {
    return new Response("Bad request", { status: 400 });
  }
  const cleanPath = pathname === "/" ? "/index.html" : pathname;
  const fileUrl = new URL(`.${cleanPath}`, dist);
  const file = Bun.file(fileUrl);
  if (await file.exists()) {
    return new Response(file, {
      headers: {
        "content-type": contentType(cleanPath),
        "cache-control": cleanPath === "/index.html" ? "no-store" : "public, max-age=31536000, immutable",
      },
    });
  }
  return new Response(Bun.file(new URL("./index.html", dist)), {
    headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" },
  });
}

Bun.serve({
  port,
  async fetch(request: Request) {
    const url = new URL(request.url);
    if (url.pathname === "/health" || url.pathname === "/healthz") {
      return Response.json({ status: "ok" });
    }
    if (url.pathname.startsWith("/api/monitoring")) {
      if (!apiKey) {
        return Response.json({ detail: "MONITORING_OPERATOR_API_KEY is not configured" }, { status: 503 });
      }
      const targetPath = url.pathname.replace(/^\/api\/monitoring/, "/admin/monitoring");
      const upstreamUrl = `${apiUrl}${targetPath}${url.search}`;
      const headers = new Headers(request.headers);
      headers.set("x-api-key", apiKey);
      headers.delete("host");
      const upstream = await fetch(upstreamUrl, {
        method: request.method,
        headers,
        body: request.method === "GET" || request.method === "HEAD" ? undefined : request.body,
      });
      const responseHeaders = new Headers(upstream.headers);
      responseHeaders.delete("content-encoding");
      responseHeaders.delete("transfer-encoding");
      return new Response(upstream.body, {
        status: upstream.status,
        headers: responseHeaders,
      });
    }
    return serveAsset(url.pathname);
  },
});

console.log(`monitoring-ui listening on :${port}`);
