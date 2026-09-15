# Design and compatibility notes

`tolk-inspect` performs a full analysis in Rust and converts Acton's borrowed syntax and
semantic structures into one owned, immutable snapshot. The thin WASM export serializes
that snapshot once. The TypeScript facade then builds indexes for traversal and queries,
so normal lint operations do not cross the WASM boundary repeatedly.

IDs are opaque strings, deterministic for identical logical paths and source within one
snapshot, and valid only until `dispose()`. They are deliberately not Acton `FileId`,
`SymbolId`, or `TyId` values. Locations contain both zero-based UTF-16 positions and
explicit UTF-8 byte ranges.

## Compatibility target

- Analyzer internals: Acton commit `17654feb713c5824ee4cc0259b7be9b5f72898ba`
- Reported Tolk version: 1.4.2
- Compatibility corpus: Acton v1.1.0 / Tolk 1.4.1 sources and matching stdlib at commit
  `9cf4d1f410267178e943daf32b44353d99ddb6db`
- Runtime tested in the first milestone: Node.js 20 and newer

The core and WASM transport are filesystem-independent. Browser packaging is not claimed
or tested in 0.1.0; the Node package uses wasm-bindgen's Node target.

## Semantic surfaces

References retain value/type namespace and call context alongside combinable
read/write/mutate facts from Acton's `tolk-analysis`. `constantValue` evaluates constant
and enum-member symbols without losing integer precision.

Per-function control-flow graphs retain branch/loop/return/throw edge kinds, source
locations, AST links, and local read/write sets without leaking Acton's internal IDs.
They are separate from, and complementary to, the inter-function call graph. Workspace
CFGs are included by default; consumers can disable them or include dependency graphs.
See [control-flow analysis](control-flow.md) for the public contract and usage examples.

`TypeInfo` exposes the major type kind, referenced declaration, element types, and
function return type. Exact constant values belong to `constantValue`; lower-level Acton
type metadata is deliberately not part of the public contract.

Workspace diagnostics include Acton's linter findings with rule codes, severities,
annotations, help text, and structured fixes. Native `check-disable-next-line`
suppressions are honored. Dependency sources remain inspectable but are not linted.

## Known gaps

- Official C++ compiler diagnostics require a separate compiler backend and are not
  available in the current WASM build.
- Calls through function-valued locals are resolved across direct assignments, copies,
  branches, and loops. Values originating from callback parameters, function returns,
  lambdas, or containers may remain partially or wholly unresolved; `callSites()` marks
  those results with `complete: false` rather than inventing a target.
- The API is snapshot-based; incremental updates require constructing another snapshot.

## Toolchain note

Tree-sitter 0.26's wasm shim must remain on `tree-sitter-language = 0.1.7`; 0.1.8
intentionally rejects it. New Clang versions also require
`-Wno-incompatible-pointer-types` for that old shim. Both constraints are encoded in the
workspace manifest/build script and should be revisited together during an Acton upgrade.
