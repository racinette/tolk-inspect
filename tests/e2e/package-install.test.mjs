import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const root = resolve(import.meta.dirname, "../..");
const packageMetadata = JSON.parse(readFileSync(join(root, "packages/tolk-inspect/package.json"), "utf8"));
const packageName = packageMetadata.name;
const tarballName = `${packageName.replace(/^@/, "").replaceAll("/", "-")}-${packageMetadata.version}.tgz`;
const consumer = mkdtempSync(join(tmpdir(), "tolk-inspect-consumer-"));
process.on("exit", () => rmSync(consumer, { recursive: true, force: true }));
const packages = join(consumer, "packages");
mkdirSync(packages);

run("npm", ["pack", join(root, "packages/tolk-inspect"), "--pack-destination", packages], root);
const tarball = join(packages, tarballName);
assert.ok(existsSync(tarball), "npm pack did not create the expected tarball");
writeFileSync(join(consumer, "package.json"), JSON.stringify({ private: true, type: "module" }));
run("npm", ["install", "--ignore-scripts", "--no-audit", "--no-fund", tarball], consumer);

const installed = join(consumer, "node_modules", packageName);
assert.ok(existsSync(join(installed, "generated/tolk_inspect_wasm_bg.wasm")), "tarball omitted WASM");
assert.ok(existsSync(join(installed, "dist/index.d.ts")), "tarball omitted declarations");
assert.ok(existsSync(join(installed, "THIRD_PARTY_NOTICES.md")), "tarball omitted notices");
assert.match(readFileSync(join(installed, "dist/index.d.ts"), "utf8"), /inspectProject/);
assert.match(readFileSync(join(installed, "dist/index.d.ts"), "utf8"), /ReferenceAccess/);
assert.match(readFileSync(join(installed, "dist/index.d.ts"), "utf8"), /constantValue/);
assert.match(readFileSync(join(installed, "dist/index.d.ts"), "utf8"), /controlFlow/);

writeFileSync(join(consumer, "consumer.mjs"), `
  import assert from "node:assert/strict";
  import { inspectProject, versionInfo } from "tolk-inspect";
  const project = await inspectProject({
    root: "/project",
    files: {
      "/project/main.tolk": 'import "messages";\\nfun main() { helper(); }',
      "/project/messages.tolk": "const ANSWER = 7; fun helper(): int { return ANSWER; }"
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
  const answer = project.symbols().find((symbol) => symbol.name === "ANSWER");
  assert.deepEqual(project.constantValue(answer), {
    kind: "int",
    value: "7",
    display: "7 (0x7)",
  });
  const helperCfg = project.controlFlow(helper);
  assert.ok(helperCfg);
  assert.equal(helperCfg.isReachable(helperCfg.exit), true);
  assert.equal(helperCfg.dominates(helperCfg.entry, helperCfg.exit), true);
  assert.equal(versionInfo().actonRevision, "16d49e1f6ad68d67072b95c77ad9175c34ad7e17");
  assert.equal(versionInfo().packageVersion, ${JSON.stringify(packageMetadata.version)});
  console.log("installed-package e2e passed");
`);
run(process.execPath, [join(consumer, "consumer.mjs")], consumer);

writeFileSync(join(consumer, "consumer.ts"), `
  import { inspectProject, type ProjectInput, type SymbolInfo } from ${JSON.stringify(packageName)};

  const input: ProjectInput = {
    root: "/project",
    files: { "/project/main.tolk": "fun main() {}" },
  };

  async function inspect(): Promise<readonly SymbolInfo[]> {
    const project = await inspectProject(input);
    const symbols = project.symbols();
    project.dispose();
    return symbols;
  }

  void inspect();
`);
writeFileSync(join(consumer, "tsconfig.json"), JSON.stringify({
  compilerOptions: {
    target: "ES2022",
    module: "NodeNext",
    moduleResolution: "NodeNext",
    strict: true,
    noEmit: true,
    skipLibCheck: false,
  },
  include: ["consumer.ts"],
}));
run(process.execPath, [join(root, "node_modules/typescript/bin/tsc"), "-p", "tsconfig.json"], consumer);

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
