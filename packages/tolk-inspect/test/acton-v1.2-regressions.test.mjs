import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { inspectProject } from "../dist/index.js";

function fixture(name) {
  return readFileSync(new URL(
    `../../../fixtures/projects/acton-v1.2-regressions/${name}.tolk`, import.meta.url,
  ), "utf8");
}

test("Acton 1.2 retains match-arm statements in CFGs and callable flow", async () => {
  const project = await inspectProject({
    root: "/virtual",
    files: {
      "/virtual/main.tolk": fixture("match-statements"),
      "/virtual/stdlib/common.tolk": "type int = builtin\ntype bool = builtin\n",
    },
    stdlibRoot: "/virtual/stdlib",
  });
  try {
    assert.deepEqual(project.diagnostics().filter((diagnostic) =>
      diagnostic.phase === "parse" || diagnostic.phase === "resolution"), []);
    const global = (name) => {
      const symbol = project.symbols().find((symbol) => symbol.name === name && !symbol.flags.local);
      assert.ok(symbol, name);
      return symbol;
    };
    const flow = global("flow");
    const cfg = project.controlFlow(flow);
    assert.ok(cfg);
    for (const kind of ["loopBack", "trueBranch", "falseBranch", "return", "throw"]) {
      assert.ok(cfg.edges.some((edge) => edge.kind === kind), kind);
    }
    const x = project.symbols().find((symbol) =>
      symbol.name === "x" && symbol.containingSymbol === flow.id);
    assert.ok(x);
    assert.ok(cfg.nodes.some((node) => node.reads.includes(x.id)));
    const write = cfg.nodes.find((node) => node.writes.includes(x.id));
    assert.ok(write?.location && write.astNodeId);
    assert.equal(cfg.isReachable(write), true);
    assert.match(project.node(write.astNodeId).text, /x \+= 1/);
    const condition = cfg.nodes.find((node) =>
      node.kind === "condition" && project.node(node.astNodeId)?.text === "x < 2");
    assert.ok(condition);
    assert.deepEqual(new Set(cfg.successors(condition).map((edge) => edge.kind)),
      new Set(["trueBranch", "falseBranch"]));
    assert.ok(cfg.predecessors(cfg.exit).some((edge) => edge.kind === "return"));

    const targets = new Set([global("first").id, global("second").id]);
    const indirect = project.callSites(global("matched")).find((site) => site.dispatch === "indirect");
    assert.ok(indirect);
    assert.equal(indirect.complete, true);
    assert.deepEqual(new Set(indirect.targets), targets);
    const direct = project.callSites(global("insideArm"));
    assert.equal(direct.length, 2);
    assert.ok(direct.every((site) => site.complete && site.dispatch === "direct"));
    assert.deepEqual(new Set(direct.flatMap((site) => site.targets)), targets);
  } finally {
    project.dispose();
  }
});

test("Acton 1.2 parses enum separators, empty tuples, unions, and escaped triple strings", async () => {
  const project = await inspectProject({
    root: "/virtual",
    files: { "/virtual/main.tolk": fixture("parser-fixes") },
  });
  try {
    assert.deepEqual(project.diagnostics().filter((diagnostic) => diagnostic.phase === "parse"), []);
    for (const name of ["Mode", "First", "Second", "Third", "Fourth", "Empty", "Nested", "Choice",
      "ESCAPED_END", "ESCAPED_MIDDLE", "ESCAPED_ONE_QUOTE", "ESCAPED_TWO_QUOTES"]) {
      assert.ok(project.symbols().some((symbol) => symbol.name === name), name);
    }
    const fourth = project.symbols().find((symbol) => symbol.name === "Fourth");
    assert.equal(project.constantValue(fourth)?.value, "4");
    for (const name of ["ESCAPED_END", "ESCAPED_MIDDLE", "ESCAPED_ONE_QUOTE", "ESCAPED_TWO_QUOTES"]) {
      const symbol = project.symbols().find((symbol) => symbol.name === name);
      assert.match(project.node(symbol.nodeId).text, /"""/);
    }
  } finally {
    project.dispose();
  }
});

const oldStdlib = 'type int = builtin\ntype slice = builtin\nfun ton(value: slice): int builtin\n';
const newStdlib = `${oldStdlib}fun grams(value: slice): int builtin\n`;
for (const { name, source, stdlib, expected, fixable } of [
  {
    name: "offers an automatic grams replacement with a modern stdlib",
    source: 'fun main(): int { return ton("1"); }',
    stdlib: newStdlib,
    expected: true,
    fixable: true,
  },
  {
    name: "does not offer an unsafe replacement when a local shadows grams",
    source: 'fun main(): int { val grams = 1; return ton("1"); }',
    stdlib: newStdlib,
    expected: true,
    fixable: false,
  },
  {
    name: "does not suggest grams for older standard libraries",
    source: 'fun main(): int { return ton("1"); }',
    stdlib: oldStdlib,
    expected: false,
  },
  {
    name: "does not rename a user-defined ton function",
    source: 'fun ton(value: slice): int { return 1; }\nfun main(): int { return ton("1"); }',
    stdlib: 'type int = builtin\ntype slice = builtin\nfun grams(value: slice): int builtin\n',
    expected: false,
  },
]) {
  test(`Acton 1.2 S009 ${name}`, async () => {
    const project = await inspectProject({
      root: "/virtual",
      files: { "/virtual/main.tolk": source, "/virtual/stdlib/common.tolk": stdlib },
      stdlibRoot: "/virtual/stdlib",
    });
    try {
      const diagnostic = project.diagnostics().find((diagnostic) => diagnostic.code === "S009");
      assert.equal(diagnostic !== undefined, expected);
      if (diagnostic) {
        assert.equal(diagnostic.phase, "lint");
        assert.equal(diagnostic.severity, "warning");
        assert.equal(diagnostic.fixes.length > 0, fixable);
        if (fixable) {
          const edit = diagnostic.fixes.find((fix) => fix.applicability === "automatic")?.edits[0];
          assert.ok(edit);
          assert.equal(edit.replacement, "grams");
          assert.equal(edit.location.path, "/virtual/main.tolk");
          assert.equal(source.slice(edit.location.byteRange.start, edit.location.byteRange.end), "ton");
        }
      }
    } finally {
      project.dispose();
    }
  });
}
