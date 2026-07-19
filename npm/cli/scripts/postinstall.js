const fs = require("node:fs");
const https = require("node:https");
const path = require("node:path");
const crypto = require("node:crypto");
const { execFileSync } = require("node:child_process");

if (process.platform !== "darwin" || process.arch !== "arm64") {
  console.error("@agboxhq/cli currently ships macOS arm64 only.");
  process.exit(1);
}

const packageRoot = path.join(__dirname, "..");
const metadata = JSON.parse(fs.readFileSync(path.join(packageRoot, "native.json"), "utf8"));
const cache = path.join(packageRoot, "cache");
const executable = path.join(cache, "agbox-darwin-arm64");
const MAX_BYTES = 128 * 1024 * 1024;
const ALLOWED_HOSTS = new Set(["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"]);

function verified(binary) {
  if (!fs.existsSync(binary) || fs.lstatSync(binary).isSymbolicLink()) return false;
  const hash = crypto.createHash("sha256");
  const descriptor = fs.openSync(binary, "r");
  const chunk = Buffer.allocUnsafe(64 * 1024);
  try {
    let bytesRead;
    do {
      bytesRead = fs.readSync(descriptor, chunk, 0, chunk.length, null);
      if (bytesRead > 0) hash.update(chunk.subarray(0, bytesRead));
    } while (bytesRead > 0);
  } finally {
    fs.closeSync(descriptor);
  }
  return hash.digest("hex") === metadata.sha256;
}

function download(url, destination, redirects = 0) {
  const parsed = new URL(url);
  if (parsed.protocol !== "https:" || !ALLOWED_HOSTS.has(parsed.hostname) || redirects > 3) {
    throw new Error("native binary URL is not allowed");
  }
  return new Promise((resolve, reject) => {
    const request = https.get(parsed, { timeout: 15_000 }, (response) => {
      if ([301, 302, 303, 307, 308].includes(response.statusCode)) {
        response.resume();
        return resolve(download(new URL(response.headers.location, parsed).toString(), destination, redirects + 1));
      }
      if (response.statusCode !== 200) {
        response.resume();
        return reject(new Error(`native binary download failed (${response.statusCode})`));
      }
      let received = 0;
      const output = fs.createWriteStream(destination, { flags: "wx", mode: 0o700 });
      response.on("data", (chunk) => {
        received += chunk.length;
        if (received > MAX_BYTES) request.destroy(new Error("native binary exceeds 128 MiB"));
      });
      response.pipe(output);
      output.on("finish", () => output.close(resolve));
      output.on("error", reject);
    });
    request.setTimeout(120_000, () => request.destroy(new Error("native binary download timed out")));
    request.on("error", reject);
  });
}

async function install() {
  fs.mkdirSync(cache, { recursive: true, mode: 0o700 });
  if (!verified(executable)) {
    const temporary = path.join(cache, `.agbox-${process.pid}-${Date.now()}.tmp`);
    try {
      await download(metadata.url, temporary);
      if (!verified(temporary)) throw new Error("native binary checksum mismatch");
      fs.chmodSync(temporary, 0o700);
      fs.renameSync(temporary, executable);
    } finally {
      if (fs.existsSync(temporary)) fs.unlinkSync(temporary);
    }
  }
  const initOutput = execFileSync(executable, ["init", "--quiet"], {
    stdio: "pipe",
    encoding: "utf8",
  });
  if (initOutput.trim()) {
    console.warn(initOutput.trim());
  }
  console.log("agbox: local Rust runtime initialized; run `agbox doctor` to verify");
}

install().catch((err) => {
  const detail = String(err.stderr || err.stdout || err.message || "").trim();
  console.error(`agbox: native runtime install failed${detail ? ` — ${detail}` : ""}`);
  process.exitCode = 1;
});
