#!/usr/bin/env node
/* global console, process */

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const root = process.cwd();
const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "codux-prepare-release-test-"));
const notesPath = path.join(tempDir, "release-notes.md");

const result = spawnSync("node", ["apps/desktop/scripts/release/prepare-release.mjs", "v1.8.0", "--dry-run"], {
  cwd: root,
  env: {
    ...process.env,
    RELEASE_NOTES_PATH: notesPath,
  },
  encoding: "utf8",
});

assert.equal(result.status, 0, result.stderr || result.stdout);
const notes = fs.readFileSync(notesPath, "utf8");
assert.match(notes, /## Downloads \/ 下载说明/);
assert.match(notes, /\| File \/ 文件 \| Usage \| 用途 \|/);
assert.match(
  notes,
  /\| \[`codux-macos-aarch64\.dmg`\]\(https:\/\/github\.com\/duxweb\/codux\/releases\/latest\/download\/codux-macos-aarch64\.dmg\) \| Apple Silicon Mac stable release \| Apple Silicon Mac 正式版本 \|/,
);
assert.match(
  notes,
  /\| \[`codux-macos-x86_64\.dmg`\]\(https:\/\/github\.com\/duxweb\/codux\/releases\/latest\/download\/codux-macos-x86_64\.dmg\) \| Intel Mac stable release \| Intel Mac 正式版本 \|/,
);
assert.match(
  notes,
  /\| \[`codux-windows-x86_64-setup\.exe`\]\(https:\/\/github\.com\/duxweb\/codux\/releases\/latest\/download\/codux-windows-x86_64-setup\.exe\) \| Windows 64-bit installer \| Windows 64 位安装包 \|/,
);
assert.match(
  notes,
  /\| \[`codux-macos-aarch64`\]\(https:\/\/github\.com\/duxweb\/codux\/releases\/latest\/download\/codux-macos-aarch64\) \| Apple Silicon Mac headless agent \| Apple Silicon Mac 主机端 \|/,
);
assert.match(
  notes,
  /\| \[`codux-macos-x86_64`\]\(https:\/\/github\.com\/duxweb\/codux\/releases\/latest\/download\/codux-macos-x86_64\) \| Intel Mac headless agent \| Intel Mac 主机端 \|/,
);
assert.match(
  notes,
  /\| \[`codux-linux-x86_64`\]\(https:\/\/github\.com\/duxweb\/codux\/releases\/latest\/download\/codux-linux-x86_64\) \| Linux x86_64 headless agent \| Linux x86_64 主机端 \|/,
);
assert.match(
  notes,
  /\| \[`codux-linux-aarch64`\]\(https:\/\/github\.com\/duxweb\/codux\/releases\/latest\/download\/codux-linux-aarch64\) \| Linux ARM64 headless agent \| Linux ARM64 主机端 \|/,
);
assert.match(
  notes,
  /\| \[`codux-windows-x86_64\.exe`\]\(https:\/\/github\.com\/duxweb\/codux\/releases\/latest\/download\/codux-windows-x86_64\.exe\) \| Windows 64-bit headless agent \| Windows 64 位主机端 \|/,
);
assert.doesNotMatch(notes, /codux-agent-1\.8\.0/);
assert.doesNotMatch(notes, /debug/);
assert.doesNotMatch(notes, /codux-\*/);
assert.doesNotMatch(notes, /latest\.json/);
assert.doesNotMatch(notes, /updater\.app\.tar\.gz/);

const rcChannelResult = spawnSync(
  "node",
  ["apps/desktop/scripts/release/prepare-release.mjs", "v2.0.0-rc.1", "--dry-run"],
  {
    cwd: root,
    env: {
      ...process.env,
      RELEASE_NOTES_PATH: path.join(tempDir, "rc-release-notes.md"),
    },
    encoding: "utf8",
  },
);

assert.equal(rcChannelResult.status, 0, rcChannelResult.stderr || rcChannelResult.stdout);
assert.match(rcChannelResult.stdout, /^channel=stable$/m);

const missingNotesResult = spawnSync(
  "node",
  ["apps/desktop/scripts/release/prepare-release.mjs", "v9.9.9-beta.99", "--dry-run"],
  {
    cwd: root,
    env: {
      ...process.env,
      RELEASE_NOTES_PATH: path.join(tempDir, "missing-release-notes.md"),
    },
    encoding: "utf8",
  },
);

assert.notEqual(missingNotesResult.status, 0);
assert.match(
  missingNotesResult.stderr,
  /Missing release notes for 9\.9\.9-beta\.99/,
);
assert.ok(!fs.existsSync(path.join(tempDir, "missing-release-notes.md")));

const fixtureRoot = path.join(tempDir, "independent-versioning");
for (const directory of [
  "apps/desktop",
  "apps/desktop/runtime",
  "apps/agent",
  "apps/wrapper-helper",
  "apps/mobile",
]) {
  fs.mkdirSync(path.join(fixtureRoot, directory), { recursive: true });
}
fs.writeFileSync(
  path.join(fixtureRoot, "apps/desktop/Cargo.toml"),
  '[package]\nname = "codux"\nversion = "1.0.0"\n',
);
fs.writeFileSync(
  path.join(fixtureRoot, "apps/desktop/runtime/Cargo.toml"),
  '[package]\nname = "codux-runtime"\nversion = "1.0.1"\n',
);
fs.writeFileSync(
  path.join(fixtureRoot, "apps/agent/Cargo.toml"),
  '[package]\nname = "codux-agent"\nversion = "1.0.2"\n',
);
fs.writeFileSync(
  path.join(fixtureRoot, "apps/wrapper-helper/Cargo.toml"),
  '[package]\nname = "codux-wrapper-helper"\nversion = "1.0.3"\n',
);
fs.writeFileSync(
  path.join(fixtureRoot, "apps/mobile/pubspec.yaml"),
  "version: 1.0.4+5\n",
);
fs.writeFileSync(
  path.join(fixtureRoot, "Cargo.lock"),
  [
    '[[package]]\nname = "codux"\nversion = "1.0.0"',
    '[[package]]\nname = "codux-runtime"\nversion = "1.0.1"',
    '[[package]]\nname = "codux-agent"\nversion = "1.0.2"',
    '[[package]]\nname = "codux-wrapper-helper"\nversion = "1.0.3"',
  ].join("\n\n") + "\n",
);
for (const changelog of ["CHANGELOG.md", "CHANGELOG.zh-CN.md"]) {
  fs.writeFileSync(
    path.join(fixtureRoot, changelog),
    "## [3.4.5-beta.6]\n\n- fixture\n",
  );
}

const independentVersionResult = spawnSync(
  "node",
  [path.join(root, "apps/desktop/scripts/release/prepare-release.mjs"), "v3.4.5-beta.6", "beta"],
  {
    cwd: fixtureRoot,
    env: {
      ...process.env,
      RELEASE_NOTES_PATH: path.join(fixtureRoot, "dist", "release-notes.md"),
    },
    encoding: "utf8",
  },
);

assert.equal(
  independentVersionResult.status,
  0,
  independentVersionResult.stderr || independentVersionResult.stdout,
);
assert.match(
  fs.readFileSync(path.join(fixtureRoot, "apps/desktop/Cargo.toml"), "utf8"),
  /version = "3\.4\.5-beta\.6"/,
);
assert.match(
  fs.readFileSync(path.join(fixtureRoot, "apps/desktop/runtime/Cargo.toml"), "utf8"),
  /version = "1\.0\.1"/,
);
assert.match(
  fs.readFileSync(path.join(fixtureRoot, "apps/agent/Cargo.toml"), "utf8"),
  /version = "1\.0\.2"/,
);
assert.match(
  fs.readFileSync(path.join(fixtureRoot, "apps/wrapper-helper/Cargo.toml"), "utf8"),
  /version = "1\.0\.3"/,
);
assert.match(
  fs.readFileSync(path.join(fixtureRoot, "apps/mobile/pubspec.yaml"), "utf8"),
  /version: 1\.0\.4\+5/,
);
const fixtureLock = fs.readFileSync(path.join(fixtureRoot, "Cargo.lock"), "utf8");
assert.match(fixtureLock, /name = "codux"\nversion = "3\.4\.5-beta\.6"/);
assert.match(fixtureLock, /name = "codux-runtime"\nversion = "1\.0\.1"/);
assert.match(fixtureLock, /name = "codux-agent"\nversion = "1\.0\.2"/);
assert.match(fixtureLock, /name = "codux-wrapper-helper"\nversion = "1\.0\.3"/);

const agentWorkflow = fs.readFileSync(
  path.join(root, ".github/workflows/release-agent.yml"),
  "utf8",
);
assert.doesNotMatch(agentWorkflow, /prepare-release\.mjs/);
assert.match(agentWorkflow, /apps\/agent\/Cargo\.toml/);

fs.rmSync(tempDir, { recursive: true, force: true });
console.log("prepare-release notes test passed");
