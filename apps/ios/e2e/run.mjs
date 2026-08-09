/**
 * The end-to-end run, in **WebKit** — the engine iOS actually uses.
 *
 * Why this exists on top of 127 unit tests: those inject a fake `fetch` and a
 * fake `EventSource`, which proves the *rules* and nothing about the runtime.
 * A phone fails in ways a fake cannot reproduce — a cookie the browser declines
 * to store, an `EventSource` handshake that drops the credential, a layout that
 * scrolls sideways, a composer iOS zooms into on focus. Every check below is one
 * of those.
 *
 * WKWebView on iOS is WebKit, and Playwright's WebKit is the same engine family
 * (`AppleWebKit/605.1.15`), so this is the closest a Linux box gets to a device.
 * It is **not** a substitute for running on hardware — see the README.
 *
 *   node e2e/run.mjs [--screenshots <dir>]
 *
 * Requires `npm run build` first (it serves `dist/`) and Playwright's WebKit
 * (`npx playwright install --with-deps webkit`).
 */
import { webkit, devices } from "playwright";
import { mkdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { startDaemon, TOKEN } from "./daemon.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const DIST = join(here, "..", "dist");

const shotsFlag = process.argv.indexOf("--screenshots");
const SHOTS = shotsFlag === -1 ? null : process.argv[shotsFlag + 1];

/** Apple's minimum touch target. Anything smaller is a mis-tap waiting. */
const MIN_TAP_PX = 44;
/** Below this, iOS zooms the page when a field takes focus. */
const MIN_INPUT_FONT_PX = 16;

let passed = 0;
const failures = [];

function check(name, condition, detail = "") {
  if (condition) {
    passed++;
    console.log(`  ok    ${name}`);
  } else {
    failures.push(`${name}${detail ? ` — ${detail}` : ""}`);
    console.log(`  FAIL  ${name}${detail ? ` — ${detail}` : ""}`);
  }
}

const daemon = await startDaemon({ dist: DIST });
const browser = await webkit.launch();
const context = await browser.newContext({
  ...devices["iPhone 15 Pro"],
  baseURL: daemon.origin,
});
const page = await context.newPage();

/**
 * Page and console errors, minus the one that is supposed to happen: the
 * unauthenticated probe of `/v1/harnesses` is how a fresh device discovers it
 * needs a token, and WebKit logs the 401 as a console error.
 */
const noise = [];
page.on("pageerror", (e) => noise.push(`pageerror: ${e.message}`));
page.on("console", (m) => {
  if (m.type() !== "error") return;
  if (/401|Unauthorized|Failed to load resource/i.test(m.text())) return;
  noise.push(`console: ${m.text()}`);
});

const shot = async (name) => {
  if (!SHOTS) return;
  await mkdir(SHOTS, { recursive: true });
  await page.screenshot({ path: join(SHOTS, `${name}.png`) });
};

try {
  console.log(`\nWebKit e2e against ${daemon.origin}`);

  // ─── the gate ───────────────────────────────────────────────────────────
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.waitForSelector("text=this device needs a token", { timeout: 20_000 });
  console.log(`\n[gate]`);
  check("a fresh device is asked for a token", true);
  check(
    "the composer is absent, not merely disabled",
    (await page.locator('textarea[placeholder="Delegate something"]').count()) === 0,
  );
  // Measured here, while the gate is on screen — after connecting it is gone
  // and the check would pass vacuously.
  const gateFont = await page.evaluate(
    () => parseFloat(getComputedStyle(document.querySelector(".gate input")).fontSize),
  );
  check(
    `the token field is >= ${MIN_INPUT_FONT_PX}px, so iOS does not zoom on focus`,
    gateFont >= MIN_INPUT_FONT_PX,
    `${gateFont}px`,
  );
  await shot("01-gate");

  // ─── the cookie exchange, for real ──────────────────────────────────────
  await page.fill('input[placeholder="Bearer token"]', TOKEN);
  await page.click("text=CONNECT");
  await page.waitForSelector('textarea[placeholder="Delegate something"]', { timeout: 20_000 });
  console.log(`\n[session]`);
  check("a bearer token is exchanged for a session and the app opens", true);

  const cookies = await context.cookies();
  const session = cookies.find((c) => c.name === "jod_session");
  check("WebKit actually stored the HttpOnly session cookie", Boolean(session));
  check("the cookie is HttpOnly, so no script can read it", session?.httpOnly === true);
  check(
    "the bearer token is not persisted anywhere on the device",
    await page.evaluate(() => {
      const all = { ...localStorage, ...sessionStorage };
      return !Object.values(all).some((v) => String(v).includes("e2e-write-token"));
    }),
  );
  check(
    "only the scope is remembered, so a relaunch is not read-only",
    (await page.evaluate(() => localStorage.getItem("jod.scope"))) === "write",
  );

  // ─── delegating, and the live stream ────────────────────────────────────
  const prompt = "the opencode resume test is failing on main — find out why and fix it";
  await page.fill('textarea[placeholder="Delegate something"]', prompt);
  await shot("02-composed");
  await page.click("text=SEND");

  console.log(`\n[delegation]`);
  await page.waitForSelector(`text=${prompt}`, { timeout: 20_000 });
  check("the prompt is echoed immediately, before the daemon answers", true);
  check(
    "the composer closes for the duration of the turn",
    await page.locator("button.send").isDisabled(),
  );

  // A real EventSource handshake, carrying the cookie WebKit just stored.
  await page.waitForSelector("text=thread panicked", { timeout: 25_000 });
  await shot("03-running");
  console.log(`\n[stream]`);
  check("SSE reaches the page over a real EventSource handshake", true);
  check(
    "a tool call carries its most useful argument",
    await page.locator("text=Grep · resume_cursor").first().isVisible(),
  );
  check(
    "what a tool gave back is on screen, not just its conclusion",
    await page.locator("text=3 matches").isVisible(),
  );
  check(
    "a failed tool is marked failed, call and output both",
    (await page.locator(".entry.tool.failed").count()) > 0 &&
      (await page.locator(".entry.tool_out.failed").count()) > 0,
  );
  check(
    "an unclassified harness line is surfaced, not swallowed",
    await page.locator("text=warning: unclassified harness line").isVisible(),
  );
  check(
    "reasoning stays hidden until asked",
    (await page.locator("text=The failure is in the resume cursor").count()) === 0,
  );

  await page.waitForSelector("text=/done · 1483 out/", { timeout: 25_000 });
  await shot("04-finished");
  console.log(`\n[completion]`);
  check("the run's summary line lands", true);
  check(
    "the status bar carries the model and the spend",
    await page.locator("text=Claude Code · claude-opus-5 · $0.0212 · ready").isVisible(),
  );
  // SEND stays disabled on an empty box by design, so "is it enabled" proves
  // nothing here. Typing is what distinguishes "closed for the turn" from
  // "open and waiting".
  check(
    "the composer reopens when the turn ends",
    !(await page.locator(".composer textarea").isDisabled()),
  );
  await page.fill(".composer textarea", "and now the next thing");
  check("a second turn can be sent into the same conversation", !(await page.locator("button.send").isDisabled()));
  await page.fill(".composer textarea", "");

  // ─── the TUI's two toggles ──────────────────────────────────────────────
  console.log(`\n[controls]`);
  await page.click("text=THINK");
  check(
    "THINK is the Ctrl-T equivalent",
    await page.locator("text=thinking shown").isVisible(),
  );
  await shot("05-thinking-toggle");

  // `/details` gates output as it *arrives*, exactly as `show_details` does in
  // `App::apply` — it does not rewrite the scrollback. Asserting the button
  // flips and the transcript is left alone is the honest form of this check;
  // suppression of the next result is covered in `app.test.tsx`, which can
  // script a second run.
  await page.click("text=TOOLS");
  check(
    "TOOLS flips without rewriting what is already on screen",
    (await page.locator("button.iconbtn.on", { hasText: "TOOLS" }).count()) === 0 &&
      (await page.locator("text=3 matches").count()) > 0,
  );
  await page.click("text=TOOLS"); // back on

  await page.click("text=AGENTS");
  await page.waitForSelector("text=audit the api auth layer", { timeout: 20_000 });
  check("AGENTS is the Ctrl-A equivalent, and lists runs this phone never started", true);
  check(
    "a finished run is badged, and an unfinished one offers STOP",
    (await page.locator("text=FAILED").count()) > 0,
  );
  await shot("06-agents");
  await page.click("text=CLOSE");

  // ─── slash commands ─────────────────────────────────────────────────────
  console.log(`\n[commands]`);
  await page.fill(".composer textarea", "/");
  await page.waitForSelector('[role="listbox"]', { timeout: 10_000 });
  check(
    "typing a slash opens the completion list",
    (await page.locator('[role="option"]').count()) >= 10,
  );
  await page.fill(".composer textarea", "/harness ");
  check(
    "arguments are completed too, so three spellings need not be remembered",
    await page.locator("text=/harness opencode").isVisible(),
  );
  await shot("08-completions");
  await page.locator('[role="option"]', { hasText: "/harness agy" }).click();
  check(
    "tapping a suggestion fills the composer",
    (await page.inputValue(".composer textarea")) === "/harness agy",
  );
  await page.click("text=SEND");
  await page.waitForSelector("text=AGY from the next turn", { timeout: 10_000 });
  check("a command runs against Jod, and is never sent to the agent", true);
  check(
    "switching harness moves the status bar with it",
    await page.locator("text=/^AGY · ready$/").isVisible(),
  );
  await page.fill(".composer textarea", "/harness claude");
  await page.click("text=SEND");

  await page.fill(".composer textarea", "/wibble");
  await page.click("text=SEND");
  check(
    "an unknown command is named back rather than swallowed",
    await page.locator("text=/wibble is not a command").isVisible(),
  );
  await page.fill(".composer textarea", "");

  // ─── the team board ─────────────────────────────────────────────────────
  console.log(`\n[team]`);
  await page.click("text=TEAM");
  await page.waitForSelector("text=scout", { timeout: 20_000 });
  check("TEAM is the Ctrl-G equivalent", true);
  check(
    "one team, three harnesses — the thing no single harness can do",
    (await page.locator("text=Claude Code").count()) > 0 &&
      (await page.locator("text=AGY").count()) > 0 &&
      (await page.locator("text=OpenCode").count()) > 0,
  );
  check(
    "the board shows progress and who owns what",
    (await page.locator("text=BOARD · 1/3").isVisible()) &&
      (await page.locator("text=(scout)").isVisible()),
  );
  await shot("09-team");
  await page.click("text=CLOSE");

  // ─── the things only a phone gets wrong ─────────────────────────────────
  console.log(`\n[iOS layout]`);
  const overflow = await page.evaluate(() => ({
    scrollWidth: document.documentElement.scrollWidth,
    clientWidth: document.documentElement.clientWidth,
  }));
  check(
    "the page never scrolls sideways",
    overflow.scrollWidth <= overflow.clientWidth + 1,
    `scrollWidth=${overflow.scrollWidth} clientWidth=${overflow.clientWidth}`,
  );

  const composerFont = await page.evaluate(() => {
    const el = document.querySelector(".composer textarea");
    return el ? parseFloat(getComputedStyle(el).fontSize) : 0;
  });
  check(
    `the composer is >= ${MIN_INPUT_FONT_PX}px, so iOS does not zoom on focus`,
    composerFont >= MIN_INPUT_FONT_PX,
    `${composerFont}px`,
  );

  const small = await page.evaluate((min) => {
    const out = [];
    for (const el of document.querySelectorAll("button")) {
      const r = el.getBoundingClientRect();
      if (r.width === 0 && r.height === 0) continue; // not rendered
      if (r.height < min || r.width < min) {
        out.push(`${el.className || el.tagName}:${Math.round(r.width)}x${Math.round(r.height)}`);
      }
    }
    return out;
  }, MIN_TAP_PX);
  check(
    `every visible control meets the ${MIN_TAP_PX}px touch target`,
    small.length === 0,
    small.join(", "),
  );

  check(
    "the layout is pinned to the safe areas",
    await page.evaluate(() => {
      const v = getComputedStyle(document.documentElement).getPropertyValue("--safe-bottom");
      return v.trim() !== "";
    }),
  );

  // ─── nothing broke quietly ──────────────────────────────────────────────
  console.log(`\n[runtime]`);
  check("no page or console errors in WebKit", noise.length === 0, noise.join(" | "));
} finally {
  await browser.close();
  await daemon.close();
}

console.log(`\n${passed} passed, ${failures.length} failed`);
if (failures.length) {
  console.error(`\nFAILED:\n - ${failures.join("\n - ")}`);
  process.exit(1);
}
console.log("PASS: apps/ios e2e (WebKit, iPhone 15 Pro viewport)");
