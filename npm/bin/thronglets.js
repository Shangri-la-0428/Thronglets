#!/usr/bin/env node
"use strict";

const { execFileSync } = require("child_process");
const path = require("path");
const fs = require("fs");

const candidates =
  process.platform === "win32"
    ? ["thronglets-bin.exe", "thronglets-bin"]
    : ["thronglets-bin"];
const binPath = candidates
  .map((name) => path.join(__dirname, name))
  .find((candidate) => fs.existsSync(candidate));

if (!binPath) {
  console.error("Thronglets binary not found. Run: npm rebuild thronglets");
  process.exit(1);
}

try {
  execFileSync(binPath, process.argv.slice(2), { stdio: "inherit" });
} catch (err) {
  process.exit(err.status || 1);
}
