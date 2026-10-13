import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { inspectProject } from "../dist/index.js";

const STDLIB = "type int = builtin\ntype bool = builtin\ntype slice = builtin\ntype bitsN = builtin\n";

function fixture(name) {
  return readFileSync(new URL(
    `../../../fixtures/projects/acton-v1.2.1-regressions/${name}.tolk`, import.meta.url,
  ), "utf8");
}

async function openFixture(name) {
  return inspectProject({
    root: "/virtual",
    files: { "/virtual/main.tolk": fixture(name), "/virtual/stdlib/common.tolk": STDLIB },
    stdlibRoot: "/virtual/stdlib",
  });
}

function global(project, name) {
  const symbol = project.symbols().find((symbol) => symbol.name === name && !symbol.flags.local);
  assert.ok(symbol, name);
  return symbol;
}

function noSemanticDiagnostics(project) {
  assert.deepEqual(project.diagnostics().filter((diagnostic) =>
    diagnostic.phase === "parse" || diagnostic.phase === "resolution"), []);
}

test("Acton 1.2.1 follows break and continue through nested loops in CFGs", async () => {
  const project = await openFixture("loop-control");
  try {
    noSemanticDiagnostics(project);
    const flow = project.controlFlow(global(project, "flow"));
    assert.ok(flow);
    const targets = (kind) => flow.edges.filter((edge) => edge.kind === kind).map((edge) => edge.to);
    const continues = targets("continue");
    assert.equal(continues.length, 2);
    assert.notEqual(continues[0], continues[1]);
    const nodeText = (id) => project.node(flow.nodes.find((node) => node.id === id)?.astNodeId)?.text;
    assert.ok(continues.some((id) => nodeText(id) === "flag > 0"));
    const breaks = targets("break");
    assert.equal(breaks.length, 2);
    assert.notEqual(breaks[0], breaks[1]);
    const total = project.symbols().find((symbol) =>
      symbol.name === "total" && symbol.containingSymbol === global(project, "flow").id);
    const increment = flow.nodes.find((node) =>
      node.writes.includes(total.id) && project.node(node.astNodeId)?.text === "total += 1");
    assert.ok(increment);
    assert.equal(flow.isReachable(increment), true);
    assert.ok(flow.predecessors(flow.exit).some((edge) => edge.kind === "return"));

    const exitEarly = global(project, "exitEarly");
    const cfg = project.controlFlow(exitEarly);
    const x = project.symbols().find((symbol) => symbol.name === "x" && symbol.containingSymbol === exitEarly.id);
    const writes = cfg.nodes.filter((node) => node.writes.includes(x.id))
      .map((node) => [project.node(node.astNodeId)?.text, cfg.isReachable(node)]);
    assert.deepEqual(new Map(writes).get("x = 4"), true);
    assert.deepEqual(new Map(writes).get("x = flag"), false);
  } finally {
    project.dispose();
  }
});

test("Acton 1.2.1 narrows a nullable local inside its null check", async () => {
  const project = await openFixture("smart-casts");
  try {
    noSemanticDiagnostics(project);
    const uses = project.files().flatMap((file) => [...file.ast.descendants("identifier")])
      .filter((node) => node.text === "value")
      .sort((a, b) => a.location.byteRange.start - b.location.byteRange.start)
      .map((node) => project.typeOf(node)?.display)
      .filter(Boolean);
    assert.ok(uses.length >= 2, JSON.stringify(uses));
    assert.equal(uses[0], "int?");
    assert.equal(uses.at(-1), "int");
  } finally {
    project.dispose();
  }
});

test("Acton 1.2.1 treats a declared bytesN alias as a user-defined type with methods", async () => {
  const project = await openFixture("bits-alias");
  try {
    noSemanticDiagnostics(project);
    const digests = project.files().flatMap((file) => [...file.ast.descendants("identifier")])
      .filter((node) => node.text === "digest")
      .map((node) => project.typeOf(node))
      .filter(Boolean);
    assert.ok(digests.length > 0);
    for (const type of digests) {
      assert.equal(type.display, "bytes4");
      assert.equal(type.kind, "typeAlias");
      assert.equal(type.symbolId, global(project, "bytes4").id);
    }
    const isValid = project.symbols().find((symbol) => symbol.name === "isValid");
    const [site] = project.callSites(global(project, "check"));
    assert.ok(site?.complete);
    assert.deepEqual(site.targets, [isValid.id]);
    assert.ok(!project.symbols().some((symbol) => symbol.name === "bytesN"));
  } finally {
    project.dispose();
  }
});
