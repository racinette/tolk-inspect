import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { inspectProject, versionInfo } from "../dist/index.js";

const packageMetadata = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));

const input = {
  root: "/virtual",
  files: {
    "/virtual/main.tolk": "import \"lib\";\nfun main() { /* 😀 */ answer(); }",
    "/virtual/lib.tolk": "fun answer(): int { return 42; }",
    "/virtual/broken.tolk": "fun broken( { return 1; }",
  },
  entrypoints: ["/virtual/main.tolk", "/virtual/broken.tolk"],
};

test("exposes an owned semantic project facade", async () => {
  const project = await inspectProject(input);
  const functions = project.files().flatMap((file) => [...file.ast.descendants("functionDeclaration")]);
  assert.equal(functions.length, 3);

  const answer = project.symbols().find((symbol) => symbol.name === "answer");
  assert.ok(answer);
  const reference = project.references(answer).find((item) => item.context.usage === "call");
  assert.ok(reference?.nodeId);
  assert.equal(project.resolve(reference.nodeId)?.symbol?.id, answer.id);
  assert.equal(project.callers(answer).length, 1);
  assert.match(project.typeOf(project.node(answer.nodeId))?.display ?? "", /int/);
  assert.ok(project.diagnostics().some((diagnostic) => diagnostic.phase === "parse"));

  const call = project.callers(answer)[0];
  assert.equal(call.callSite.range.start.character, 22);
  assert.equal(call.callSite.byteRange.start, 38);
  assert.ok(call.callSite.byteRange.start > call.callSite.range.start.character);

  project.dispose();
  assert.throws(() => project.files(), /disposed/);
});

test("reports the exact analyzer revision", () => {
  assert.deepEqual(versionInfo(), {
    packageVersion: packageMetadata.version,
    actonRevision: "16d49e1f6ad68d67072b95c77ad9175c34ad7e17",
    tolkVersion: "1.4.2",
  });
});

test("exposes Acton linter diagnostics with structured fixes", async () => {
  const project = await inspectProject({
    root: "/virtual",
    files: {
      "/virtual/main.tolk": "fun main() {\n  val unused = 1;\n}",
    },
  });
  const diagnostic = project.diagnostics().find((item) => item.code === "E001");
  assert.ok(diagnostic);
  assert.equal(diagnostic.phase, "lint");
  assert.equal(diagnostic.source, "tolk-linter");
  assert.equal(diagnostic.location?.path, "/virtual/main.tolk");
  assert.equal(diagnostic.help, undefined);
  assert.ok(diagnostic.annotations.some((annotation) =>
    annotation.primary && annotation.tags.includes("unnecessary")));
  assert.ok(diagnostic.fixes.some((fix) =>
    fix.applicability === "automatic"
      && fix.edits.some((edit) => edit.replacement === "_unused")));
});

test("exposes combinable read, write, and mutate reference facts", async () => {
  const project = await inspectProject({
    root: "/virtual",
    files: {
      "/virtual/main.tolk": `
struct Counter {
  value: int
}

fun Counter.increment(mutate self) {
  self.value += 1;
}

fun main() {
  var counter = Counter { value: 0 };
  counter.increment();
  val copy = counter;
  counter = Counter { value: 1 };
}`,
    },
  });

  const counter = project.symbols().find((symbol) => symbol.name === "counter");
  assert.ok(counter);
  const references = project.references(counter);
  assert.ok(references.some((reference) =>
    reference.context.usage === "call"
      && reference.context.access.read
      && reference.context.access.write
      && reference.context.access.mutate));
  assert.ok(references.some((reference) =>
    reference.context.usage === "read"
      && reference.context.access.read
      && !reference.context.access.write));
  assert.ok(references.some((reference) =>
    reference.context.usage === "write"
      && !reference.context.access.read
      && reference.context.access.write
      && !reference.context.access.mutate));
});

test("evaluates constants and enum members with exact integers", async () => {
  const project = await inspectProject({
    root: "/virtual",
    files: {
      "/virtual/main.tolk": `
const BASE = 10;
const VALUE = (BASE + 2) * 3;
const HUGE = 18446744073709551616;
const ENABLED = true;
const LABEL = "tolk";
const TOO_BIG = 1 << 256;
const NONE = null;

enum Mode {
  First = VALUE,
  Second,
}`,
    },
  });

  const value = project.symbols().find((symbol) => symbol.name === "VALUE");
  const huge = project.symbols().find((symbol) => symbol.name === "HUGE");
  const enabled = project.symbols().find((symbol) => symbol.name === "ENABLED");
  const label = project.symbols().find((symbol) => symbol.name === "LABEL");
  const tooBig = project.symbols().find((symbol) => symbol.name === "TOO_BIG");
  const none = project.symbols().find((symbol) => symbol.name === "NONE");
  const second = project.symbols().find((symbol) => symbol.name === "Second");
  assert.ok(value && huge && enabled && label && tooBig && none && second);
  assert.deepEqual(project.constantValue(value), {
    kind: "int",
    value: "36",
    display: "36 (0x24)",
  });
  assert.equal(project.constantValue(huge)?.value, "18446744073709551616");
  assert.deepEqual(project.constantValue(enabled), { kind: "bool", value: true, display: "true" });
  assert.deepEqual(project.constantValue(label), { kind: "string", value: "tolk", display: '"tolk"' });
  assert.deepEqual(project.constantValue(tooBig), { kind: "overflow", display: "overflow" });
  assert.deepEqual(project.constantValue(none), { kind: "unknown", display: "unknown" });
  assert.equal(project.constantValue(second)?.value, "37");
  assert.equal(project.constantValue("not-a-symbol"), undefined);
});

test("exposes navigable control flow with reachability and dominance helpers", async () => {
  const source = `
fun guarded(mutate balance: int, isOwner: bool, amount: int) {
  assert (isOwner) throw 401;
  if (amount > 0) {
    balance -= amount;
  }
}

fun conditionallyGuarded(mutate balance: int, isOwner: bool, amount: int) {
  if (amount > 0) {
    assert (isOwner) throw 401;
  }
  balance -= amount;
}`;
  const project = await inspectProject({
    root: "/virtual",
    files: { "/virtual/main.tolk": source },
  });
  const guarded = project.symbols().find((symbol) => symbol.name === "guarded");
  assert.ok(guarded);
  const balance = project.symbols().find((symbol) =>
    symbol.name === "balance" && symbol.containingSymbol === guarded.id);
  assert.ok(balance);
  const cfg = project.controlFlow(guarded);
  assert.ok(cfg);

  const check = cfg.nodes.find((node) => node.kind === "assert");
  const write = cfg.nodes.find((node) => node.writes.includes(balance.id));
  const condition = cfg.nodes.find((node) => node.kind === "condition");
  assert.ok(check?.astNodeId && write?.location && condition);
  assert.equal(project.node(check.astNodeId)?.text.includes("isOwner"), true);
  assert.equal(cfg.dominates(check, write), true);
  assert.equal(cfg.postDominates(cfg.exit, cfg.entry), true);
  assert.equal(cfg.postDominates(write, check), false);
  assert.equal(cfg.isReachable(write), true);
  assert.equal(cfg.reachableFrom(cfg.entry).length, cfg.nodes.length);
  assert.deepEqual(
    new Set(cfg.successors(condition).map((edge) => edge.kind)),
    new Set(["trueBranch", "falseBranch"]),
  );
  assert.ok(cfg.predecessors(cfg.exit).length > 0);
  assert.equal(project.controlFlowGraphs().length, 2);

  const conditional = project.symbols().find((symbol) => symbol.name === "conditionallyGuarded");
  assert.ok(conditional);
  const conditionalBalance = project.symbols().find((symbol) =>
    symbol.name === "balance" && symbol.containingSymbol === conditional.id);
  const conditionalCfg = project.controlFlow(conditional);
  const conditionalCheck = conditionalCfg?.nodes.find((node) => node.kind === "assert");
  const conditionalWrite = conditionalCfg?.nodes.find((node) =>
    conditionalBalance !== undefined && node.writes.includes(conditionalBalance.id));
  assert.ok(conditionalCfg && conditionalCheck && conditionalWrite);
  assert.equal(conditionalCfg.dominates(conditionalCheck, conditionalWrite), false);

  const withoutCfg = await inspectProject({
    root: "/virtual",
    files: { "/virtual/main.tolk": source },
    controlFlow: "none",
  });
  assert.equal(withoutCfg.controlFlowGraphs().length, 0);
});

test("resolves indirect calls through local callable values", async () => {
  const project = await inspectProject({
    root: "/virtual",
    files: {
      "/virtual/main.tolk": `
fun first(value: int): int { return value; }
fun second(value: int): int { return value + 1; }

fun choose(flag: bool, value: int): int {
  var action = first;
  if (flag) {
    action = second;
  }
  val alias = action;
  return alias(value);
}

fun invoke(callback: (int) -> int, value: int): int {
  return callback(value);
}`,
    },
    controlFlow: "none",
  });

  assert.equal(project.controlFlowGraphs().length, 0);
  const choose = project.symbols().find((symbol) => symbol.name === "choose");
  const first = project.symbols().find((symbol) => symbol.name === "first");
  const second = project.symbols().find((symbol) => symbol.name === "second");
  const invoke = project.symbols().find((symbol) => symbol.name === "invoke");
  assert.ok(choose && first && second && invoke);

  const resolved = project.callSites(choose).find((callSite) => callSite.dispatch === "indirect");
  assert.ok(resolved);
  assert.equal(resolved.complete, true);
  assert.deepEqual(new Set(resolved.targets), new Set([first.id, second.id]));
  assert.deepEqual(
    new Set(project.calls(choose).map((edge) => `${edge.dispatch}:${edge.callee}`)),
    new Set([`indirect:${first.id}`, `indirect:${second.id}`]),
  );

  const unknown = project.callSites(invoke)[0];
  assert.ok(unknown);
  assert.equal(unknown.dispatch, "indirect");
  assert.equal(unknown.complete, false);
  assert.deepEqual(unknown.targets, []);
  assert.equal(project.calls(invoke).length, 0);
  assert.ok(project.callSites().length >= 2);
});
