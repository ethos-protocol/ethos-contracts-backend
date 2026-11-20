import { test } from "node:test";
import assert from "node:assert/strict";
import { EthosApiClient } from "./client.js";

test("keeps the bearer token and content type when callers pass headers (#906)", async () => {
  const seen: Headers[] = [];
  const original = globalThis.fetch;
  globalThis.fetch = (async (_url: unknown, init?: RequestInit) => {
    seen.push(new Headers(init?.headers));
    return new Response("{}");
  }) as typeof fetch;

  try {
    const client = new EthosApiClient({ baseUrl: "http://localhost:3000", token: "tok" });
    await client.getHealth({ headers: { "x-trace-id": "abc" } });
  } finally {
    globalThis.fetch = original;
  }

  assert.equal(seen[0].get("authorization"), "Bearer tok");
  assert.equal(seen[0].get("content-type"), "application/json");
  assert.equal(seen[0].get("x-trace-id"), "abc");
});
