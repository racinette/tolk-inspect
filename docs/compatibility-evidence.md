# Acton 1.1 compatibility evidence

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
