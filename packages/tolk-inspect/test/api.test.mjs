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
