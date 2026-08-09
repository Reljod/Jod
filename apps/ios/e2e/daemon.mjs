/**
 * A stand-in `jod-api`, just real enough to drive the built app end to end:
 * same-origin static assets, a cookie session, and a per-agent SSE stream that
 * replays a scripted run.
 *
 * It exists because the unit suites inject a fake `fetch` and a fake
 * `EventSource`. Those prove the rules; they cannot prove that a real browser
 * sets a real cookie on a real `POST /v1/session` and then carries it on a real
 * `EventSource` handshake — which is exactly the part that breaks on a device.
 *
 * Deliberately dumb: it replays what it is told to. Anything clever here would
 * be a second implementation of the daemon, and the e2e run would start passing
 * against a fiction.
 */
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";

export const TOKEN = "e2e-write-token";

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml",
};

/**
 * One scripted delegation, covering every event kind core can emit — including
 * `raw`, which is the harness-upgrade seam and must reach the screen.
 */
const SCRIPT = [
  { kind: "started", session_id: "sess-4f1c", model: "claude-opus-5" },
  { kind: "thinking", text: "The failure is in the resume cursor, not the parser." },
  { kind: "message", text: "Looking at the harness adapter first." },
  { kind: "tool_call", name: "Grep" },
  { kind: "tool_result", name: "Grep", is_error: false },
  { kind: "tool_call", name: "Bash" },
  { kind: "tool_result", name: "Bash", is_error: true },
  { kind: "raw", line: "warning: unclassified harness line" },
  {
    kind: "message",
    text: "Fixed in core/src/harness/opencode.rs and re-ran the suite: 214 passed.",
  },
  { kind: "finished", is_error: false, usage: { output_tokens: 1483, cost_usd: 0.0212 } },
];

function summary({ id, name, status, cost = 0 }) {
  return {
    id,
    name,
    harness: "claude_code",
    harness_label: "Claude Code",
    status,
    cwd: "/srv/jod",
    model: "claude-opus-5",
    permission: "accept_edits",
    tmux_session: `jod-${id}`,
    attach_command: `tmux attach -t jod-${id}`,
    switch_command: `tmux switch-client -t jod-${id}`,
    session_closed: false,
    created_at_ms: 1_754_700_000_000,
    session_id: null,
    usage: { cost_usd: cost },
    event_count: 0,
    last_message: null,
    stream_path: `/root/.jod/runs/${id}/stream.jsonl`,
  };
}

/** Start the daemon. Resolves with `{ origin, close }`. */
export async function startDaemon({ dist, stepMs = 60 }) {
  let agents = [
    summary({ id: "a-earlier", name: "audit the api auth layer", status: "completed", cost: 0.0431 }),
    summary({ id: "a-oldest", name: "draft the vps runbook", status: "failed", cost: 0.009 }),
  ];

  const authed = (req) => (req.headers.cookie ?? "").includes("jod_session=");
  const problem = (res, status, detail) => {
    res.writeHead(status, { "content-type": "application/problem+json" });
    res.end(JSON.stringify({ type: "about:blank", title: "Error", status, detail }));
  };
  const json = (res, body) => {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(body));
  };
  const read = (req) =>
    new Promise((resolve) => {
      let s = "";
      req.on("data", (d) => (s += d)).on("end", () => resolve(s || "{}"));
    });
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  const server = createServer(async (req, res) => {
    const path = new URL(req.url, "http://localhost").pathname;

    if (path === "/v1/session" && req.method === "POST") {
      if ((req.headers.authorization ?? "") !== `Bearer ${TOKEN}`) {
        return problem(res, 401, "unauthorized");
      }
      // No `Secure` here: the e2e origin is plain http on loopback, and a
      // Secure cookie would simply never be stored. The daemon sets it.
      res.writeHead(201, {
        "content-type": "application/json",
        "set-cookie": "jod_session=e2e; Path=/; HttpOnly; SameSite=Strict",
      });
      return res.end(JSON.stringify({ scope: "write", expires_at_ms: 2_000_000_000_000 }));
    }

    if (path.startsWith("/v1/")) {
      if (!authed(req)) return problem(res, 401, "unauthorized");

      if (path === "/v1/harnesses") {
        return json(res, [
          { id: "claude_code", label: "Claude Code", available: true, path: "/usr/local/bin/claude" },
          { id: "open_code", label: "OpenCode", available: true, path: "/root/.opencode/bin/opencode" },
          { id: "agy", label: "AGY", available: false, path: null },
        ]);
      }

      if (path === "/v1/agents" && req.method === "GET") return json(res, agents);

      if (path === "/v1/agents" && req.method === "POST") {
        const body = JSON.parse(await read(req));
        const agent = summary({ id: "a-live", name: body.name ?? "agent", status: "running" });
        agents = [agent, ...agents];
        res.writeHead(201, { "content-type": "application/json", location: "/v1/agents/a-live" });
        return res.end(JSON.stringify(agent));
      }

      const stream = path.match(/^\/v1\/agents\/([^/]+)\/stream$/);
      if (stream) {
        res.writeHead(200, {
          "content-type": "text/event-stream",
          "cache-control": "no-cache",
          connection: "keep-alive",
        });
        let seq = 0;
        for (const event of SCRIPT) {
          await sleep(stepMs);
          const envelope = { ...event, agent_id: stream[1], at_ms: 1_754_700_000_000 + seq, seq: seq++ };
          res.write(`id: ${envelope.seq}\nevent: agent\ndata: ${JSON.stringify(envelope)}\n\n`);
          if (event.kind === "finished") {
            agents = agents.map((a) => (a.id === stream[1] ? { ...a, status: "completed" } : a));
          }
        }
        return;
      }

      if (/^\/v1\/agents\/[^/]+\/events$/.test(path)) {
        return json(res, { events: [], last_seq: null });
      }
      if (/^\/v1\/agents\/[^/]+$/.test(path) && req.method === "DELETE") {
        res.writeHead(204);
        return res.end();
      }
      return problem(res, 404, "no such route");
    }

    // Static assets, same-origin — so the cookie behaves as it will in
    // production, where the daemon serves the app itself.
    try {
      const rel = path === "/" ? "/index.html" : path;
      const file = join(dist, normalize(rel).replace(/^(\.\.[/\\])+/, ""));
      const body = await readFile(file);
      res.writeHead(200, { "content-type": MIME[extname(file)] ?? "application/octet-stream" });
      res.end(body);
    } catch {
      res.writeHead(404);
      res.end("not found");
    }
  });

  // Port 0 lets the OS pick a free one, so parallel runs cannot collide.
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();

  return {
    origin: `http://127.0.0.1:${port}`,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}
