import assert from "node:assert/strict";
import test from "node:test";
import { inspectProject, versionInfo } from "../dist/index.js";

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
    packageVersion: "0.1.0",
    actonRevision: "17654feb713c5824ee4cc0259b7be9b5f72898ba",
    tolkVersion: "1.4.2",
  });
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
