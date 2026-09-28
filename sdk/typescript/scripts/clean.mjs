import { rmSync } from "node:fs";

for (const dir of ["dist", "build-test"]) {
  rmSync(new URL(`../${dir}`, import.meta.url), { recursive: true, force: true });
}
