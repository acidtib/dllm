const MIME: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
  ".json": "application/json",
  ".woff2": "font/woff2",
};

async function proxy(url: string): Promise<Response> {
  const resp = await fetch(url);
  const headers = new Headers(resp.headers);
  return new Response(resp.body, { status: resp.status, headers });
}

Bun.serve({
  port: 5173,
  async fetch(req) {
    const { pathname } = new URL(req.url);

    // Proxy API and health
    if (pathname.startsWith("/v1") || pathname === "/health") {
      return proxy(`http://127.0.0.1:7337${pathname}`);
    }

    // Serve static files from dist/
    let file = Bun.file(`./dist${pathname}`);
    if (await file.exists()) {
      const ext = pathname.match(/\.\w+$/)?.[0] ?? "";
      return new Response(file, {
        headers: { "Content-Type": MIME[ext] ?? "application/octet-stream" },
      });
    }

    // SPA fallback
    file = Bun.file("./dist/index.html");
    return new Response(file, {
      headers: { "Content-Type": "text/html; charset=utf-8" },
    });
  },
});

console.log("Dev server running at http://localhost:5173");
