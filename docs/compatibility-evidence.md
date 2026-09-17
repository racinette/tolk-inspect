# Acton compatibility evidence

## Stable Acton 1.2 analyzer backend

The current backend uses the `v1.2.0` tag, resolved in `Cargo.lock` to
`16d49e1f6ad68d67072b95c77ad9175c34ad7e17`. Runtime `versionInfo()` reports that exact
revision and analyzer Tolk version 1.4.2.

The focused corpus under `fixtures/projects/acton-v1.2-regressions` is exercised by native
Rust tests and public Node/WASM tests. It checks direct match-arm statements through
resolution, local accesses, CFG edges/navigation, and direct/indirect call targets. It
also checks enum semicolon separators, empty/nested tuple types, leading-pipe
parenthesized types, and escaped triple-string delimiters. Separate tests check `S009`
diagnostics and automatic fixes with modern stdlibs, local shadowing, old stdlibs, and
user-defined functions.

These checks validate the analyzer and package, not execution by the official v1.2.0 C++
compiler. The older upstream corpus and compiler evidence below remain deliberately
versioned as v1.1.0 backward compatibility.

## Acton 1.1 compiler and corpus evidence

The corpus under `fixtures/upstream/acton-v1.1.0` is copied from the exact v1.1.0 commit,
`9cf4d1f410267178e943daf32b44353d99ddb6db`, together with that release's Tolk 1.4.1
standard library.

On 2026-09-14, the official Linux x86-64 Acton v1.1.0 release binary reported:

```text
acton 1.1.0 (9cf4d1f 2026-05-22)
```

It successfully compiled the copied `tests/projects/basic` fixture. The repeatable check is
`ACTON_V1_1_BIN=/path/to/acton npm run test:compat`.

The installed-package test analyzes sources through the generated WASM and verifies known
cross-file references and call sites. The Acton counter-template test uses the matching
v1.1.0 stdlib, asserts zero parse diagnostics, traverses over 4,000 AST nodes, observes over
300 local definitions, resolves `Storage` across files, checks inferred `Storage` type data,
and verifies the resolved `Storage.load` call edge.
