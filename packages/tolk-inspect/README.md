# tolk-inspect

Read-only AST and semantic inspection for in-memory Tolk projects, backed by Rust and
WebAssembly.

`tolk-inspect` is designed for audit tools, linters, code navigation, and visualizations.
It resolves project-wide symbols and types, evaluates constants, exposes control-flow
graphs, traces direct and indirect calls, and reports Acton linter diagnostics.

## Install

```bash
npm install tolk-inspect
```

The package is ESM-only, runs on Node.js 22 or newer, and has no runtime dependencies.
Browser bundling is not currently supported.

## Quickstart

```ts
import { inspectProject } from "tolk-inspect";

const project = await inspectProject({
  root: "/project",
  files: {
    "/project/main.tolk": `
      import "math";
      fun main(): int { return answer(); }
    `,
    "/project/math.tolk": `
      const VALUE = 42;
      fun answer(): int { return VALUE; }
    `,
  },
  entrypoints: ["/project/main.tolk"],
});

const answer = project.symbols().find((symbol) => symbol.name === "answer");
if (answer) {
  console.log("references", project.references(answer));
  console.log("callers", project.callers(answer));
  console.log("CFG", project.controlFlow(answer));
}

console.log("diagnostics", project.diagnostics());
project.dispose();
```

All paths are logical, normalized project paths. The analyzer never reads the filesystem;
the caller supplies every workspace, Tolk standard-library, and Acton-library source.
For a complete project, pass those sources together with their roots:

```ts
const project = await inspectProject({
  root: "/project",
  files: {
    ...workspaceSources,
    ...tolkStandardLibrarySources,
    ...actonLibrarySources,
  },
  entrypoints: ["/project/contracts/main.tolk"],
  stdlibRoot: "/project/.acton/tolk-stdlib",
  actonStdlibRoot: "/project/.acton",
  importMappings: { "@acton": "/project/.acton" },
});
```

## Main API

An `InspectedProject` provides:

- `files()`, `node()`, and AST traversal through `AstNode`
- `symbols()`, `symbolFor()`, `symbolAt()`, `resolve()`, and `references()`
- `typeOf()` and exact `constantValue()` results
- `controlFlow()`, including successors, predecessors, reachability, and dominance
- `callSites()`, `callGraph()`, `calls()`, and `callers()`
- `diagnostics()`, including structured Acton linter fixes

`callSites()` is the authoritative representation for indirect calls. A call may have
several conservative targets; `complete: false` means an additional source-level target
could not be identified.

Call `dispose()` when the snapshot is no longer needed. Access after disposal throws.

## Analysis boundaries

- Snapshots are immutable; analyze again after changing source text.
- Official C++ compiler diagnostics are not part of the WASM package.
- Callables originating in opaque FFI, runtime, or deserialized data may remain incomplete.
- Callable analysis is intentionally whole-program and context-insensitive, so a helper
  called with different callbacks reports their conservative union.

## Guides and compatibility

- [Full project README](https://github.com/racinette/tolk-inspect#readme)
- [Control-flow analysis](https://github.com/racinette/tolk-inspect/blob/main/docs/control-flow.md)
- [Call-site and call-graph analysis](https://github.com/racinette/tolk-inspect/blob/main/docs/call-graph.md)
- [Callable-flow coverage](https://github.com/racinette/tolk-inspect/blob/main/docs/callable-flow-audit.md)
- [Design and known boundaries](https://github.com/racinette/tolk-inspect/blob/main/docs/design.md)

The analysis backend uses stable Acton `v1.2.0`, locked to commit
`16d49e1f6ad68d67072b95c77ad9175c34ad7e17`. Use `versionInfo()` to inspect the package,
Acton revision, and analyzer Tolk version at runtime.
