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

## Known gaps

- Semantic diagnostics currently include project/import failures and unresolved names;
  Acton's complete compiler diagnostic set is not yet exposed.
- Reference context distinguishes calls and type/value namespaces. Its combinable
  read/write/mutate access flags come from Acton's `tolk-analysis` crate.
- Call sites cover statically resolved direct functions and methods. Calls through
  function-valued locals have no global callee and therefore produce no call edge.
- The API is snapshot-based; incremental updates require constructing another snapshot.
- Type structure exposes the major type kind, referenced declaration, element types, and
  function return type. Literal refinements and every internal Acton type detail are not
  part of the public contract.

## Toolchain note

Tree-sitter 0.26's wasm shim must remain on `tree-sitter-language = 0.1.7`; 0.1.8
intentionally rejects it. New Clang versions also require
`-Wno-incompatible-pointer-types` for that old shim. Both constraints are encoded in the
workspace manifest/build script and should be revisited together during an Acton upgrade.
