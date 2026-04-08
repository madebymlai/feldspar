#!/usr/bin/env node

const { execFileSync } = require("child_process");
const path = require("path");
const os = require("os");

const PLATFORMS = {
  "linux-x64": "@feldspar/linux-x64",
  "linux-arm64": "@feldspar/linux-arm64",
  "darwin-x64": "@feldspar/darwin-x64",
  "darwin-arm64": "@feldspar/darwin-arm64",
  "win32-x64": "@feldspar/win32-x64",
};

function getBinaryPath() {
  const platform = os.platform();
  const arch = os.arch();
  const key = `${platform}-${arch}`;
  const pkg = PLATFORMS[key];

  if (!pkg) {
    console.error(`Unsupported platform: ${key}`);
    process.exit(1);
  }

  try {
    const pkgPath = require.resolve(`${pkg}/package.json`);
    const binName = platform === "win32" ? "feldspar.exe" : "feldspar";
    return path.join(path.dirname(pkgPath), "bin", binName);
  } catch {
    // Fallback: try feldspar in PATH
    return "feldspar";
  }
}

const binary = getBinaryPath();
const args = process.argv.slice(2);

try {
  execFileSync(binary, args, { stdio: "inherit" });
} catch (e) {
  if (e.status !== undefined) {
    process.exit(e.status);
  }
  console.error("Failed to run feldspar:", e.message);
  process.exit(1);
}
