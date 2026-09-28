import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { SDK_VERSION } from "../src/index.js";

// Compiled to build-test/test/, so the package root is two levels up.
const root = new URL("../../", import.meta.url);
const manifest = JSON.parse(readFileSync(new URL("package.json", root), "utf8")) as {
  version: string;
  dependencies?: Record<string, string>;
  license: string;
  private: boolean;
};

test("the package has no runtime dependencies", () => {
  assert.equal(manifest.dependencies, undefined);
  assert.equal(manifest.license, "Apache-2.0");
  assert.equal(manifest.private, false);
});

test("SDK_VERSION matches package.json", () => {
  assert.equal(SDK_VERSION, manifest.version);
});

test("the CommonJS and ESM builds load and export the same names", async () => {
  const require = createRequire(import.meta.url);
  const cjs = require(fileURLToPath(new URL("dist/cjs/index.js", root))) as Record<string, unknown>;
  const esm = (await import(new URL("dist/esm/index.js", root).href)) as Record<string, unknown>;
  const names = (module: Record<string, unknown>) => Object.keys(module).filter((name) => name !== "default").sort();
  assert.deepEqual(names(cjs), names(esm));
  for (const name of ["GatewayClient", "GatewayError", "verifyWebhook", "formatTokenAmount", "parseMinorUnits", "newIdempotencyKey"]) {
    assert.equal(typeof esm[name], "function", name);
  }
  assert.equal((cjs["formatTokenAmount"] as (raw: string, decimals: number) => string)("4999000", 6), "4.999");
});
