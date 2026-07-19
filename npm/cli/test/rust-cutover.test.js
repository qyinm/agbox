const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const root = path.resolve(__dirname, "..", "..", "..");
const tracked = execFileSync("git", ["-C", root, "ls-files"], { encoding: "utf8" })
  .split("\n")
  .filter(Boolean);

test("shipping tree contains only the Rust runtime", () => {
  assert.equal(tracked.some((file) => file.endsWith(".go")), false);
  assert.equal(tracked.includes("go.mod"), false);
  assert.equal(tracked.includes("go.sum"), false);
  assert.equal(tracked.includes("npm/cli/dist/agbox-darwin-arm64"), false);
});

test("npm launcher has no legacy dist fallback", () => {
  const launcher = fs.readFileSync(path.join(root, "npm/cli/bin/agbox"), "utf8");
  assert.equal(launcher.includes("dist/agbox-darwin-arm64"), false);
  assert.match(launcher, /cache.*agbox-darwin-arm64/s);
  const packageJson = JSON.parse(fs.readFileSync(path.join(root, "npm/cli/package.json"), "utf8"));
  assert.equal(packageJson.scripts["test:cutover"], "node --test test/rust-cutover.test.js");
});
