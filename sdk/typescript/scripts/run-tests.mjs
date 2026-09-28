// Node 18 and 20 take a directory after --test, Node 22 takes globs; an
// explicit file list works on all of them and never matches zero files silently.
import { spawnSync } from "node:child_process";
import { readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

const dir = fileURLToPath(new URL("../build-test/test/", import.meta.url));
const files = readdirSync(dir)
  .filter((name) => name.endsWith(".test.js"))
  .sort()
  .map((name) => `${dir}${name}`);
if (files.length === 0) {
  console.error(`no compiled tests in ${dir}`);
  process.exit(1);
}
const result = spawnSync(process.execPath, ["--test", ...files], { stdio: "inherit" });
process.exit(result.status ?? 1);
