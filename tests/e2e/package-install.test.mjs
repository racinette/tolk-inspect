import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const root = resolve(import.meta.dirname, "../..");
const consumer = mkdtempSync(join(tmpdir(), "tolk-inspect-consumer-"));
process.on("exit", () => rmSync(consumer, { recursive: true, force: true }));
const packages = join(consumer, "packages");
mkdirSync(packages);

run("npm", ["pack", join(root, "packages/tolk-inspect"), "--pack-destination", packages], root);
const tarball = join(packages, "tolk-inspect-0.1.0.tgz");
assert.ok(existsSync(tarball), "npm pack did not create the expected tarball");
writeFileSync(join(consumer, "package.json"), JSON.stringify({ private: true, type: "module" }));
run("npm", ["install", "--ignore-scripts", "--no-audit", "--no-fund", tarball], consumer);

const installed = join(consumer, "node_modules/tolk-inspect");
assert.ok(existsSync(join(installed, "generated/tolk_inspect_wasm_bg.wasm")), "tarball omitted WASM");
assert.ok(existsSync(join(installed, "dist/index.d.ts")), "tarball omitted declarations");
assert.match(readFileSync(join(installed, "dist/index.d.ts"), "utf8"), /inspectProject/);
assert.match(readFileSync(join(installed, "dist/index.d.ts"), "utf8"), /ReferenceAccess/);

writeFileSync(join(consumer, "consumer.mjs"), `
  import assert from "node:assert/strict";
  import { inspectProject, versionInfo } from "tolk-inspect";
  const project = await inspectProject({
    root: "/project",
    files: {
      "/project/main.tolk": 'import "messages";\\nfun main() { helper(); }',
      "/project/messages.tolk": "fun helper(): int { return 7; }"
    },
    entrypoints: ["/project/main.tolk"]
  });
  const helper = project.symbols().find((symbol) => symbol.name === "helper");
  assert.ok(helper);
  assert.equal(project.callers(helper).length, 1);
  assert.equal(project.references(helper).length, 1);
  assert.deepEqual(project.references(helper)[0].context.access, {
    read: true,
    write: false,
    mutate: false,
  });
  assert.equal(versionInfo().actonRevision, "17654feb713c5824ee4cc0259b7be9b5f72898ba");
  console.log("installed-package e2e passed");
`);
run(process.execPath, [join(consumer, "consumer.mjs")], consumer);

function run(command, args, cwd) {
  const result = spawnSync(command, args, {
    cwd,
    encoding: "utf8",
    stdio: "pipe",
    env: { ...process.env, npm_config_cache: join(consumer, ".npm-cache") },
  });
  if (result.status !== 0) throw new Error(`${command} failed: ${result.error ?? "exit " + result.status}\n${result.stdout}\n${result.stderr}`);
  if (result.stdout) process.stdout.write(result.stdout);
}
