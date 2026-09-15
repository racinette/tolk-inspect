#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const packagePath = join(root, "packages/tolk-inspect/package.json");
const packageLockPath = join(root, "package-lock.json");
const cargoManifestPath = join(root, "Cargo.toml");
const cargoLockPath = join(root, "Cargo.lock");

const packageMetadata = JSON.parse(readFileSync(packagePath, "utf8"));
const packageLock = JSON.parse(readFileSync(packageLockPath, "utf8"));
const cargoManifest = readFileSync(cargoManifestPath, "utf8");
const cargoLock = readFileSync(cargoLockPath, "utf8");

const workspacePackage = section(cargoManifest, "workspace.package");
const cargoVersion = value(workspacePackage, "version", cargoManifestPath);
const npmVersion = packageMetadata.version;
const lockfileVersion = packageLock.packages?.["packages/tolk-inspect"]?.version;

assert.equal(cargoVersion, npmVersion, "Cargo workspace and npm package versions differ");
assert.equal(lockfileVersion, npmVersion, "package-lock.json and npm package versions differ");
for (const crate of ["tolk-inspect-core", "tolk-inspect-wasm"]) {
  const escaped = crate.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = cargoLock.match(new RegExp(`\\[\\[package\\]\\]\\nname = "${escaped}"\\nversion = "([^"]+)"`));
  assert.ok(match, `Cargo.lock has no ${crate} package`);
  assert.equal(match[1], npmVersion, `${crate} and npm package versions differ`);
}

const tagIndex = process.argv.indexOf("--tag");
if (tagIndex !== -1) {
  const tag = process.argv[tagIndex + 1];
  assert.ok(tag, "--tag requires a value");
  assert.equal(tag, `v${npmVersion}`, `release tag must be v${npmVersion}`);
}

console.log(`version check passed: ${packageMetadata.name}@${npmVersion}`);

function section(document, name) {
  const marker = `[${name}]`;
  const start = document.indexOf(marker);
  assert.notEqual(start, -1, `missing ${marker} section`);
  const bodyStart = start + marker.length;
  const remainder = document.slice(bodyStart);
  const nextSection = remainder.search(/^\[/m);
  return nextSection === -1 ? remainder : remainder.slice(0, nextSection);
}

function value(document, name, source) {
  const match = document.match(new RegExp(`^${name}\\s*=\\s*"([^"]+)"`, "m"));
  assert.ok(match, `missing ${name} in ${source}`);
  return match[1];
}
