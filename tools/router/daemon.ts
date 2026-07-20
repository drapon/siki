#!/usr/bin/env bun
/**
 * siki-router daemon
 *
 * ワークツリーごとに立ち上がる dev server (3000, 3001, 3002...) を
 * 常に localhost:3000 一本でアクセスできるようにするリバースプロキシ。
 *
 * - HTTP / WebSocket (HMR) を "現在アクティブなターゲット" にフォワードする。
 * - コントロールAPI (`/__siki-router__/*`) で登録・切り替えを行う。
 * - 状態は ~/.siki/router-state.json に永続化するので daemon 再起動後も復元される。
 *
 * 起動: bun run daemon.ts
 * 停止: Ctrl+C、または `siki-router stop`
 */

import { homedir } from "os";
import { join } from "path";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "fs";

const PORT = Number(process.env.SIKI_ROUTER_PORT ?? 3000);
const CONTROL_PREFIX = "/__siki-router__";
const STATE_DIR = join(homedir(), ".siki");
const STATE_FILE = join(STATE_DIR, "router-state.json");

type State = {
  target: string | null; // registry のキー (worktree 名)
  registry: Record<string, { port: number; updatedAt: string }>;
};

function loadState(): State {
  try {
    const raw = readFileSync(STATE_FILE, "utf-8");
    const parsed = JSON.parse(raw);
    return { target: parsed.target ?? null, registry: parsed.registry ?? {} };
  } catch {
    return { target: null, registry: {} };
  }
}

function saveState() {
  mkdirSync(STATE_DIR, { recursive: true });
  writeFileSync(STATE_FILE, JSON.stringify(state, null, 2));
}

const state = loadState();

function currentTargetPort(): number | null {
  if (!state.target) return null;
  return state.registry[state.target]?.port ?? null;
}

function isLoopback(server: any, req: Request): boolean {
  const addr = server.requestIP(req);
  if (!addr) return true; // unix socket 等、判定できない場合は許可
  return addr.address === "127.0.0.1" || addr.address === "::1";
}

function json(data: unknown, init: ResponseInit = {}) {
  return new Response(JSON.stringify(data, null, 2), {
    ...init,
    headers: { "content-type": "application/json; charset=utf-8", ...(init.headers ?? {}) },
  });
}

async function handleControl(req: Request, url: URL, server: any): Promise<Response> {
  if (!isLoopback(server, req)) {
    return json({ error: "forbidden: loopback only" }, { status: 403 });
  }

  const path = url.pathname.slice(CONTROL_PREFIX.length) || "/";

  if (path === "/status" && req.method === "GET") {
    return json({ target: state.target, port: currentTargetPort(), registry: state.registry });
  }

  if (path === "/register" && req.method === "POST") {
    const body = await req.json().catch(() => null);
    const name = body?.name?.toString().trim();
    const port = Number(body?.port);
    if (!name || !Number.isInteger(port) || port <= 0) {
      return json({ error: "name (string) と port (number) が必要です" }, { status: 400 });
    }
    const isFirst = Object.keys(state.registry).length === 0;
    state.registry[name] = { port, updatedAt: new Date().toISOString() };
    let switched = false;
    if (body?.switch === true || isFirst) {
      state.target = name;
      switched = true;
    }
    saveState();
    return json({ ok: true, name, port, switched, target: state.target });
  }

  if (path === "/unregister" && req.method === "POST") {
    const body = await req.json().catch(() => null);
    const name = body?.name?.toString().trim();
    if (!name || !state.registry[name]) {
      return json({ error: `未登録: ${name}` }, { status: 404 });
    }
    delete state.registry[name];
    if (state.target === name) state.target = null;
    saveState();
    return json({ ok: true });
  }

  if (path === "/switch" && req.method === "POST") {
    const body = await req.json().catch(() => null);
    const name = body?.name?.toString().trim();
    if (!name || !state.registry[name]) {
      return json({ error: `未登録: ${name}. まず register してください`, registry: state.registry }, { status: 404 });
    }
    state.target = name;
    saveState();
    return json({ ok: true, target: state.target, port: currentTargetPort() });
  }

  return json({ error: "not found" }, { status: 404 });
}

function landingPage(): Response {
  const entries = Object.entries(state.registry);
  const rows = entries
    .map(
      ([name, info]) => `
      <tr>
        <td>${name}</td>
        <td>${info.port}</td>
        <td><button onclick="switchTo('${name}')">切り替え</button></td>
      </tr>`
    )
    .join("");

  const html = `<!doctype html>
<html lang="ja">
<head><meta charset="utf-8"><title>siki-router</title>
<style>
  body { font-family: -apple-system, sans-serif; max-width: 640px; margin: 60px auto; color: #222; }
  table { width: 100%; border-collapse: collapse; margin-top: 16px; }
  td, th { padding: 8px 12px; border-bottom: 1px solid #ddd; text-align: left; }
  button { cursor: pointer; }
  code { background: #f4f4f4; padding: 2px 6px; border-radius: 4px; }
</style>
</head>
<body>
  <h1>siki-router</h1>
  <p>現在アクティブなターゲットがありません。登録済みワークスペース:</p>
  <table><thead><tr><th>name</th><th>port</th><th></th></tr></thead><tbody>
    ${rows || '<tr><td colspan="3">まだ何も登録されていません。`siki-dev` 経由で dev server を起動してください。</td></tr>'}
  </tbody></table>
  <p>CLIから: <code>siki-router switch &lt;name&gt;</code></p>
  <script>
    async function switchTo(name) {
      await fetch('${CONTROL_PREFIX}/switch', {
        method: 'POST', headers: {'content-type':'application/json'},
        body: JSON.stringify({ name })
      });
      location.reload();
    }
  </script>
</body>
</html>`;
  return new Response(html, { headers: { "content-type": "text/html; charset=utf-8" } });
}

async function proxyHttp(req: Request, port: number, url: URL): Promise<Response> {
  const target = `http://127.0.0.1:${port}${url.pathname}${url.search}`;
  const headers = new Headers(req.headers);
  headers.set("host", `127.0.0.1:${port}`);
  headers.delete("content-length"); // fetch が body から再計算する

  try {
    const resp = await fetch(target, {
      method: req.method,
      headers,
      body: ["GET", "HEAD"].includes(req.method) ? undefined : req.body,
      redirect: "manual",
      // @ts-expect-error Bun 固有: ストリーミング body 送信に必要
      duplex: "half",
    });
    const respHeaders = new Headers(resp.headers);
    respHeaders.set("x-siki-router-target", `${state.target ?? "?"}:${port}`);
    return new Response(resp.body, { status: resp.status, headers: respHeaders });
  } catch (err) {
    return new Response(
      `<h1>siki-router: proxy error</h1><p>ターゲット "${state.target}" (port ${port}) に接続できません。dev server が起動しているか確認してください。</p><pre>${String(err)}</pre>`,
      { status: 502, headers: { "content-type": "text/html; charset=utf-8" } }
    );
  }
}

type WSData = { targetPort: number; path: string; backend?: WebSocket; queue: (string | Uint8Array)[] };

const server = Bun.serve<WSData>({
  port: PORT,
  hostname: "0.0.0.0",
  async fetch(req, server) {
    const url = new URL(req.url);

    if (url.pathname === CONTROL_PREFIX || url.pathname.startsWith(CONTROL_PREFIX + "/")) {
      return handleControl(req, url, server);
    }

    const port = currentTargetPort();
    if (!port) return landingPage();

    const isWebSocket = req.headers.get("upgrade")?.toLowerCase() === "websocket";
    if (isWebSocket) {
      const ok = server.upgrade(req, {
        data: { targetPort: port, path: url.pathname + url.search, queue: [] },
      });
      if (ok) return undefined as unknown as Response;
      return new Response("WebSocket upgrade failed", { status: 500 });
    }

    return proxyHttp(req, port, url);
  },
  websocket: {
    open(ws) {
      const { targetPort, path } = ws.data;
      const backend = new WebSocket(`ws://127.0.0.1:${targetPort}${path}`);
      ws.data.backend = backend;
      backend.addEventListener("message", (e) => {
        try {
          ws.send(e.data as any);
        } catch {}
      });
      backend.addEventListener("close", () => {
        try {
          ws.close();
        } catch {}
      });
      backend.addEventListener("error", () => {
        try {
          ws.close();
        } catch {}
      });
      backend.addEventListener("open", () => {
        for (const m of ws.data.queue) backend.send(m as any);
        ws.data.queue = [];
      });
    },
    message(ws, message) {
      const backend = ws.data.backend;
      if (backend && backend.readyState === WebSocket.OPEN) {
        backend.send(message as any);
      } else {
        ws.data.queue.push(message);
      }
    },
    close(ws) {
      try {
        ws.data.backend?.close();
      } catch {}
    },
  },
});

console.log(`siki-router: listening on http://localhost:${server.port}`);
console.log(`  control API: http://localhost:${server.port}${CONTROL_PREFIX}/status`);
console.log(`  state file:  ${STATE_FILE}`);
if (state.target) {
  console.log(`  current target: ${state.target} -> :${currentTargetPort()}`);
} else {
  console.log(`  current target: (none yet)`);
}
