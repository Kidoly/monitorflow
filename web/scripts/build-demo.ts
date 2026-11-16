// Builds the static demo (docs/demo.md): the normal build with the demo flag
// on, so every API call is answered by made-up data in the browser and every
// write is refused. The files land in `out/`, like the normal build.
//
//   npm run build:demo
//
// NEXT_PUBLIC_MONITORFLOW_SITE_URL, when set, is where the banner's
// "Website" link leads.

import { spawnSync } from "node:child_process";

const result = spawnSync("npm", ["run", "build"], {
  stdio: "inherit",
  shell: process.platform === "win32",
  env: { ...process.env, NEXT_PUBLIC_MONITORFLOW_DEMO: "1" },
});
process.exit(result.status ?? 1);
