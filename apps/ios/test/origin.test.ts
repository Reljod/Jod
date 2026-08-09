/**
 * Where the daemon is.
 *
 * This module exists because of a bug the simulator found and nothing else
 * could: in the packaged app the page is served from `tauri://localhost`, so a
 * relative `/v1/harnesses` is not a valid URL and WebKit throws
 * `The string did not match the expected pattern.` — a message that names
 * neither the cause nor the fix. Every case below is a shape that mistake can
 * take.
 */

import { describe, expect, it } from "vitest";

import {
  normaliseOrigin,
  resolveBase,
  servedOverHttp,
  type OriginMemory,
} from "../src/origin";

function memory(value: string | null = null): OriginMemory & { value: string | null } {
  return {
    value,
    read() {
      return this.value;
    },
    write(origin: string) {
      this.value = origin;
    },
  };
}

describe("servedOverHttp", () => {
  it("recognises the deployments that have a same-origin daemon", () => {
    expect(servedOverHttp("http:")).toBe(true);
    expect(servedOverHttp("https:")).toBe(true);
  });

  it("rejects the schemes a packaged shell serves from", () => {
    // These are the ones where "same origin" is the app bundle, not the API.
    expect(servedOverHttp("tauri:")).toBe(false);
    expect(servedOverHttp("capacitor:")).toBe(false);
    expect(servedOverHttp("file:")).toBe(false);
    expect(servedOverHttp(undefined)).toBe(false);
  });
});

describe("normaliseOrigin", () => {
  it("assumes http for a bare host, because the tailnet is not TLS by default", () => {
    expect(normaliseOrigin("jod-cloud:8787")).toBe("http://jod-cloud:8787");
    expect(normaliseOrigin("jod-cloud")).toBe("http://jod-cloud");
    expect(normaliseOrigin("100.90.80.70:8787")).toBe("http://100.90.80.70:8787");
  });

  it("keeps a scheme that was given", () => {
    expect(normaliseOrigin("https://jod.tailnet.ts.net")).toBe(
      "https://jod.tailnet.ts.net",
    );
  });

  it("trims what a phone keyboard adds", () => {
    expect(normaliseOrigin("  jod-cloud:8787  ")).toBe("http://jod-cloud:8787");
  });

  it("drops a path, which would otherwise 404 every route", () => {
    expect(normaliseOrigin("http://jod-cloud:8787/v1")).toBe("http://jod-cloud:8787");
    expect(normaliseOrigin("http://jod-cloud:8787/")).toBe("http://jod-cloud:8787");
  });

  it("refuses what cannot be an origin", () => {
    expect(normaliseOrigin("")).toBeNull();
    expect(normaliseOrigin("   ")).toBeNull();
    expect(normaliseOrigin("http://")).toBeNull();
  });

  it("refuses a non-http scheme rather than storing a URL that will throw later", () => {
    // Storing `tauri://…` here is precisely how the original bug would come
    // back, one layer deeper and harder to see.
    expect(normaliseOrigin("tauri://localhost")).toBeNull();
    expect(normaliseOrigin("file:///tmp")).toBeNull();
    expect(normaliseOrigin("javascript:alert(1)")).toBeNull();
  });
});

describe("resolveBase", () => {
  it("uses same-origin in a browser, which is the web deployment", () => {
    // "" is a real answer — relative paths — and must not be confused with null.
    expect(resolveBase(memory(), "https:")).toBe("");
  });

  it("asks for an address in a packaged shell", () => {
    expect(resolveBase(memory(), "tauri:")).toBeNull();
  });

  it("prefers a remembered address over the current origin", () => {
    expect(resolveBase(memory("jod-cloud:8787"), "https:")).toBe(
      "http://jod-cloud:8787",
    );
  });

  it("asks again if what was remembered is unusable", () => {
    expect(resolveBase(memory("nonsense://"), "tauri:")).toBeNull();
  });
});
