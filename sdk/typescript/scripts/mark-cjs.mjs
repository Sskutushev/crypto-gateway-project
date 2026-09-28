// The package is "type": "module", so Node would read dist/cjs/*.js as ESM
// without a nearer package.json declaring CommonJS.
import { writeFileSync } from "node:fs";

writeFileSync(
  new URL("../dist/cjs/package.json", import.meta.url),
  `${JSON.stringify({ type: "commonjs" }, null, 2)}\n`,
);
