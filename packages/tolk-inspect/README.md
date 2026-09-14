# tolk-inspect

Read-only AST and semantic inspection for in-memory Tolk projects, backed by Rust and
WebAssembly. See the repository README for the complete API, examples, compatibility
policy, and build instructions.

```ts
import { inspectProject } from "tolk-inspect";

const project = await inspectProject({
  root: "/project",
  files: { "/project/main.tolk": "fun main() {}" },
});

for (const file of project.files()) {
  console.log([...file.ast.descendants("functionDeclaration")]);
}
```

Resolved references include `context.access` flags for precise read, write, and mutation
classification backed by Acton's `tolk-analysis` crate.
`project.constantValue(symbol)` evaluates constants and enum members, representing integer
values as exact decimal strings.
`project.controlFlow(symbol)` returns a navigable per-function CFG with reachability,
dominance, source-location, AST-link, and local read/write information.
`project.diagnostics()` includes Acton linter findings for workspace files with rule
codes, annotations, help text, and structured fixes. Acton
`check-disable-next-line` comments are honored.

Version 0.1.0 is tested on Node.js 20 and newer.
