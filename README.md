# tolk-inspect

`tolk-inspect` is a read-only semantic inspection library for Tolk projects. It accepts
logical paths and source text, performs parsing, project-aware resolution and type
inference in Rust/WebAssembly, and exposes an ergonomic TypeScript snapshot for lint,
audit, and visualization scripts.

The 0.1.0 package is tested on Node.js 20+. Its analysis core has no filesystem access:
callers provide every project, standard-library, and Acton-library source explicitly.

## Usage

```ts
import { inspectProject } from "tolk-inspect";

const project = await inspectProject({
  root: "/project",
  files: {
    "/project/contracts/main.tolk": mainSource,
    "/project/contracts/messages.tolk": messagesSource,
    "/project/.acton/tolk-stdlib/common.tolk": commonSource,
  },
  entrypoints: ["/project/contracts/main.tolk"],
  stdlibRoot: "/project/.acton/tolk-stdlib",
  actonStdlibRoot: "/project/.acton",
  importMappings: { "@acton": "/project/.acton" },
});

for (const file of project.files()) {
  for (const fn of file.ast.descendants("functionDeclaration")) {
    const symbol = project.symbolFor(fn);
    const directThrow = [...fn.descendants()].some(
      (node) => node.kind === "throwStatement" || node.kind === "assertStatement",
    );
    if (symbol && directThrow) {
      console.log(`${symbol.fqn} directly throws at ${symbol.declaration.path}`);
      console.log("resolved outgoing calls", project.calls(symbol));
    }
  }
}

project.dispose();
```

AST nodes have snapshot-stable IDs, normalized camel-case `kind` values, the original
Tree-sitter `rawKind`, source text, UTF-16 and byte locations, parent/child traversal,
descendant filtering, generic named fields, and conveniences including `name`, `body`,
`parameters`, `callee`, and `arguments`. Unknown grammar nodes remain visible.

The project facade provides `file`, `node`, `symbols`, `symbol`, `symbolFor`, `symbolAt`,
`resolve`, `references`, `typeOf`, `constantValue`, `controlFlow`, `controlFlowGraphs`,
`callGraph`, `calls`, `callers`, and `diagnostics`.
References include combinable `context.access.read`, `write`, and `mutate` facts computed
by Acton's `tolk-analysis`, in addition to their syntactic usage and namespace.
`constantValue` evaluates constant and enum-member symbols; integer values are decimal
strings so values outside JavaScript's safe-integer range remain exact.
`versionInfo()` reports the package, pinned Acton revision, and analyzer Tolk version.

## Build and test

Prerequisites are Rust 1.97.1 with `wasm32-unknown-unknown`, Node.js 20+, npm,
`wasm-bindgen-cli` matching the Cargo lockfile, and a WASI SDK installation.

```bash
npm install
cargo install wasm-bindgen-cli --version 0.2.128 --locked
export WASI_SDK_PATH=/opt/wasi-sdk
npm run build
npm test
```

`npm test` runs native Rust tests, rebuilds WASM and TypeScript, runs public-facade tests
against focused and Acton 1.1 fixtures, packs the npm package, installs the tarball into a
clean temporary consumer, and executes the public API there.

To repeat the release-compiler compatibility check with an installed or downloaded Acton
1.1.0 binary:

```bash
ACTON_V1_1_BIN=/path/to/acton npm run test:compat
```

Dependencies use exact Acton Git commit
`17654feb713c5824ee4cc0259b7be9b5f72898ba`; `Cargo.lock` is committed. The self-contained
compatibility corpus under `fixtures/upstream/acton-v1.1.0` uses Acton v1.1.0 and its
matching Tolk 1.4.1 stdlib. See [design and compatibility notes](docs/design.md) for gaps.
See [control-flow analysis](docs/control-flow.md) for the CFG API, common graph helpers,
and an authorization-before-mutation audit example.
