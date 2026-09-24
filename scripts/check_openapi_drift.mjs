#!/usr/bin/env node
/**
 * Issue #347 — OpenAPI ⇄ router drift check.
 *
 * Compares the `METHOD /path` surface of `docs/openapi.yaml` against the Axum
 * router in `backend/src/main.rs`. Wired into CI by
 * `.github/workflows/openapi.yml`.
 *
 *   - FAIL: a public `/api/...` route is served by the router but absent from
 *     the spec (clients would have no contract for it).
 *   - WARN: the spec declares a public operation the router does not serve yet
 *     (spec is ahead of the implementation — tracked, not blocking).
 *
 * Surface check only (path + method), matching the runtime `schema_validation`
 * middleware. Body-schema drift is out of scope.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const HTTP_METHODS = ["get", "put", "post", "delete", "patch", "head", "options"];

// Legacy routes intentionally excluded from the public OpenAPI contract
// (see the "legacy reminder / subscription routes" block in main.rs).
const LEGACY = [
  "GET /api/vaults/:p/reminders",
  "POST /api/vaults/:p/subscriptions",
  "DELETE /api/vaults/:p/subscriptions",
];

function parseSpecOperations(yaml) {
  const ops = new Set();
  let inPaths = false;
  let currentPath = null;
  for (const raw of yaml.split(/\r?\n/)) {
    const line = raw.replace(/\s+$/, "");
    if (!line.trim() || line.trim().startsWith("#")) continue;
    const indent = line.length - line.trimStart().length;
    if (indent === 0) {
      inPaths = line.startsWith("paths:");
      currentPath = null;
      continue;
    }
    if (!inPaths) continue;
    const t = line.trimStart();
    if (indent === 2 && t.startsWith("/") && t.endsWith(":")) {
      currentPath = t.slice(0, -1);
    } else if (indent === 4 && t.endsWith(":") && currentPath) {
      const key = t.slice(0, -1).toLowerCase();
      if (HTTP_METHODS.includes(key)) {
        ops.add(`${key.toUpperCase()} ${normalize(currentPath)}`);
      }
    }
  }
  return ops;
}

/** `{vault_id}` and `:vault_id` both collapse to `:p` so the styles compare. */
function normalize(path) {
  return path.replace(/\{[^}]+\}/g, ":p").replace(/:[A-Za-z0-9_]+/g, ":p");
}

/** Slice `src` from `start` to the matching close paren of a `(` at/after start. */
function balancedSlice(src, start) {
  let depth = 0;
  for (let i = start; i < src.length; i++) {
    const c = src[i];
    if (c === "(") depth++;
    else if (c === ")") {
      depth--;
      if (depth === 0) return src.slice(start, i + 1);
    }
  }
  return src.slice(start);
}

/** Pull `.route("/path", get(..).post(..))` declarations out of Rust source. */
function parseRouterOperations(src) {
  const ops = new Set();
  const head = /\.route\(\s*"([^"]+)"\s*,/g;
  let m;
  while ((m = head.exec(src)) !== null) {
    const path = normalize(m[1]);
    const body = balancedSlice(src, m.index + ".route".length);
    for (const method of HTTP_METHODS) {
      if (new RegExp(`\\b${method}\\s*\\(`, "i").test(body)) {
        ops.add(`${method.toUpperCase()} ${path}`);
      }
    }
  }
  return ops;
}

const spec = parseSpecOperations(
  readFileSync(join(repoRoot, "docs/openapi.yaml"), "utf8"),
);
const router = parseRouterOperations(
  readFileSync(join(repoRoot, "backend/src/main.rs"), "utf8"),
);

const isPublic = (op) => op.split(" ")[1].startsWith("/api/");

const specAhead = [...spec].filter(isPublic).filter((op) => !router.has(op));
const undocumented = [...router]
  .filter(isPublic)
  .filter((op) => !spec.has(op) && !LEGACY.includes(op));

if (specAhead.length) {
  console.warn("WARN: declared in openapi.yaml but not served yet:");
  for (const op of specAhead.sort()) console.warn(`  - ${op}`);
}

if (undocumented.length) {
  console.error("\nFAIL: served by the router but missing from openapi.yaml:");
  for (const op of undocumented.sort()) console.error(`  - ${op}`);
  console.error("\nAdd these operations to docs/openapi.yaml (or to the LEGACY list).");
  process.exit(1);
}

console.log(
  `OpenAPI surface in sync: ${
    [...router].filter(isPublic).length
  } router operations, 0 undocumented.`,
);
