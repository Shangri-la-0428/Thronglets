#!/usr/bin/env node
"use strict";

const fs = require("fs");
const path = require("path");
const https = require("https");
const http = require("http");

const { version: PACKAGE_VERSION } = require("../package.json");

const VERSION = process.env.THRONGLETS_INSTALL_VERSION || PACKAGE_VERSION;
const REPO = process.env.THRONGLETS_INSTALL_REPO || "Shangri-la-0428/Thronglets";

const PLATFORMS = {
  "darwin-arm64": {
    asset: `thronglets-mcp-darwin-arm64`,
    sha256: "7b0546e9381b8dc9180036afc1cbcd504068b4ac13d92f497a44945fc3faad5e",
  },
  "linux-x64": {
    asset: `thronglets-mcp-linux-amd64`,
    sha256: "d02883a6eecb861de8c1328ee9f264d4eb2d17635eb8db069bdf66ea7e9f33e6",
  },
};

const platform = `${process.platform}-${process.arch}`;
const target = PLATFORMS[platform];

if (!target) {
  console.error(`Unsupported platform: ${platform}`);
  console.error(`Supported: ${Object.keys(PLATFORMS).join(", ")}`);
  console.error("You can install from source: cargo install thronglets");
  process.exit(1);
}

const binDir = path.join(__dirname, "..", "bin");
const binPath = path.join(binDir, "thronglets-bin");
const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${target.asset}`;

function download(url, dest) {
  return new Promise((resolve, reject) => {
    const follow = (url) => {
      const client = url.startsWith("https") ? https : http;
      client.get(url, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          follow(res.headers.location);
          return;
        }
        if (res.statusCode !== 200) {
          reject(new Error(`Download failed: HTTP ${res.statusCode}`));
          return;
        }
        const file = fs.createWriteStream(dest);
        res.pipe(file);
        file.on("finish", () => {
          file.close();
          resolve();
        });
      }).on("error", reject);
    };
    follow(url);
  });
}

async function main() {
  console.log(`Downloading thronglets v${VERSION} for ${platform}...`);

  fs.mkdirSync(binDir, { recursive: true });

  try {
    await download(url, binPath);
    fs.chmodSync(binPath, 0o755);
    console.log("Thronglets installed successfully.");
  } catch (err) {
    console.error(`Failed to download: ${err.message}`);
    console.error("You can install from source: cargo install thronglets");
    process.exit(1);
  }
}

main();
