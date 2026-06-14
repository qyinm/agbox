const fs = require("node:fs");
const path = require("node:path");

if (process.platform !== "darwin" || process.arch !== "arm64") {
  console.error("@agboxhq/cli currently ships macOS arm64 only.");
  process.exit(1);
}

const executable = path.join(__dirname, "..", "dist", "agbox-darwin-arm64");
fs.chmodSync(executable, 0o755);
