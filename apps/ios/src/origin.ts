/**
 * Where the daemon is.
 *
 * The web client at `apps/web` is *served by* the daemon, so every route can be
 * relative and there is nothing to configure. The packaged iOS app is not: its
 * assets load from Tauri's own scheme (`tauri://localhost`), so a relative
 * `/v1/harnesses` resolves against that scheme and WebKit rejects it outright —
 * on the simulator it surfaces as `The string did not match the expected
 * pattern.`, which is a `SyntaxError` from the URL parser and says nothing
 * useful about the cause.
 *
 * So the address is a real setting on this client, and the app asks for it
 * rather than guessing. It is **not** a secret: it is a tailnet hostname, it
 * grants nothing on its own, and the token exchange still gates every call.
 */

const KEY = "jod.origin";

/** Somewhere to keep the address between launches. */
export interface OriginMemory {
  read(): string | null;
  write(origin: string): void;
}

export function browserOriginMemory(key = KEY): OriginMemory {
  return {
    read() {
      try {
        const value = globalThis.localStorage?.getItem(key);
        return value && value.trim() !== "" ? value : null;
      } catch {
        return null;
      }
    },
    write(origin) {
      try {
        globalThis.localStorage?.setItem(key, origin);
      } catch {
        // Not remembering is a worse session, not a broken one.
      }
    },
  };
}

/**
 * Whether this page can reach the daemon on its own origin.
 *
 * True when the app was served over http(s) — the browser deployment, and
 * `vite dev` behind its proxy. False for `tauri://`, `capacitor://`, `file://`
 * and anything else a packaged shell might use, where "same origin" is the app
 * bundle and contains no API at all.
 */
export function servedOverHttp(protocol: string | undefined): boolean {
  return protocol === "http:" || protocol === "https:";
}

/**
 * Normalise what someone typed into a base URL.
 *
 * Accepts `jod-cloud:8787`, `jod-cloud`, or a full URL; returns an origin with
 * no trailing slash, or `null` if it cannot be made into one. Bare hostnames
 * get `http://` because the daemon binds loopback and is reached over a
 * tailnet, where plain http is the normal case rather than a mistake.
 */
export function normaliseOrigin(input: string): string | null {
  const text = input.trim();
  if (text === "") return null;

  const withScheme = /^[a-z][a-z0-9+.-]*:\/\//i.test(text) ? text : `http://${text}`;
  let url: URL;
  try {
    url = new URL(withScheme);
  } catch {
    return null;
  }
  if (!servedOverHttp(url.protocol)) return null;
  if (url.hostname === "") return null;

  // Keep only the origin: a path here would be silently prepended to every
  // route and produce 404s that look like the daemon is broken.
  return url.origin;
}

/**
 * The base URL to use, or `null` when the app must ask for one.
 *
 * An empty string is a meaningful answer — it means "same origin, use relative
 * paths" — so this deliberately does not conflate it with `null`.
 */
export function resolveBase(
  memory: OriginMemory,
  protocol: string | undefined,
): string | null {
  const stored = memory.read();
  if (stored) return normaliseOrigin(stored);
  return servedOverHttp(protocol) ? "" : null;
}
