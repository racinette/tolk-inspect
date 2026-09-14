import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join, relative, resolve } from "node:path";
import test from "node:test";
import { inspectProject } from "../dist/index.js";

test("inspects the Acton 1.1 counter template with its matching stdlib", async () => {
  const fixture = resolve(import.meta.dirname, "../../../fixtures/upstream/acton-v1.1.0");
  const files = {};
  addTolkFiles(join(fixture, "src/commands/new/templates/counter/contracts"), fixture, files);
  addTolkFiles(join(fixture, "crates/tolk-compiler/assets/tolk-stdlib"), fixture, files);

  const entrypoint = "/fixture/src/commands/new/templates/counter/contracts/Counter.tolk";
  const project = await inspectProject({
    root: "/fixture",
    files,
    entrypoints: [entrypoint],
    stdlibRoot: "/fixture/crates/tolk-compiler/assets/tolk-stdlib",
  });

  assert.deepEqual(project.file(entrypoint).imports.map((item) => item.targetPath), [
    "/fixture/src/commands/new/templates/counter/contracts/types.tolk",
  ]);
  assert.equal(project.diagnostics().filter((item) => item.phase === "parse").length, 0);
  assert.ok(project.files().reduce((count, file) => count + [...file.ast.descendants()].length, 0) > 4_000);
  assert.ok(project.symbols().filter((symbol) => symbol.flags.local).length > 300);
  const contract = project.symbols().find((symbol) => symbol.kind === "contract" && symbol.name === "Counter");
  assert.ok(contract);
  assert.ok(project.symbols().some((symbol) => symbol.kind === "contractField" && symbol.containingSymbol === contract.id));

  const storage = project.symbols().find((symbol) => symbol.fqn === "Storage");
  const handler = project.symbols().find((symbol) => symbol.name === "onInternalMessage");
  assert.ok(storage && handler);
  assert.ok(project.references(storage).length >= 8);
  assert.ok(project.calls(handler).some((edge) => project.symbol(edge.callee)?.fqn === "Storage.load"));

  const declaration = project.node(storage.nodeId);
  assert.equal(declaration.kind, "structDeclaration");
  assert.match(project.typeOf(declaration)?.display ?? "", /Storage/);
});

function addTolkFiles(directory, fixture, output) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) addTolkFiles(path, fixture, output);
    else if (entry.name.endsWith(".tolk")) {
      output[`/fixture/${relative(fixture, path).replaceAll("\\", "/")}`] = readFileSync(path, "utf8");
    }
  }
}
