const fs = require("node:fs");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

if (process.platform !== "darwin" || process.arch !== "arm64") {
  console.error("@agboxhq/cli currently ships macOS arm64 only.");
  process.exit(1);
}

const executable = path.join(__dirname, "..", "dist", "agbox-darwin-arm64");
fs.chmodSync(executable, 0o755);

const truthyEnv = (key) => {
  const value = String(process.env[key] || "").trim().toLowerCase();
  return value === "1" || value === "true" || value === "yes";
};

if (truthyEnv("AGBOX_SKIP_WATCHER")) {
  console.log("agbox: watcher install skipped (AGBOX_SKIP_WATCHER=1)");
  process.exit(0);
}

try {
  execFileSync(executable, ["init", "--quiet"], { stdio: "pipe" });
  if (truthyEnv("AGBOX_SKIP_CONNECT") || truthyEnv("AGBOX_SKIP_HOOKS")) {
    console.log("agbox: watcher installed · managed hooks skipped · run `agbox doctor` to verify");
  } else {
    console.log("agbox: watcher initialized · managed proposal hooks attempted · Codex users: run /hooks and trust agbox hooks · run `agbox doctor` to verify");
  }
} catch (err) {
  console.error("agbox: watcher install failed — run `agbox init` manually");
}
