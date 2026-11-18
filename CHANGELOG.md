# Changelog

Notable changes to `tolk-inspect` are documented here. The project follows Semantic
Versioning; while the package is below 1.0, minor releases may change the public API.

## 0.1.6 - 2026-11-18

### Fixed

- A reference that only reads a value inside an object-literal field nested in another
  object literal, such as `self` in `Outer { inner: Inner { key: self.key }.toCell() }`, is
  no longer reported as a write and mutation. Acton stores an object literal's field keys out
  of source order, so the method lookup missed and was treated as a mutating call. ([#1](https://github.com/racinette/tolk-inspect/issues/1))
- An argument of a chained `mutate self` call, such as `value` in
  `beginCell().storeUint(value, 32).storeUint(1, 8)` or `entries.set(key, value).set(1, 2)`, is
  no longer reported as a write and mutation. Acton applied the next call's `mutate self` to
  every reference inside its receiver, including the previous call's arguments. ([#1](https://github.com/racinette/tolk-inspect/issues/1))
- A reference inside a call's receiver is reported as `usage: "call"` only when it lies on the
  receiver path, as `counter` does in `counter.increment()`. Values computed inside the receiver,
  such as object-literal fields, are now plain reads. ([#1](https://github.com/racinette/tolk-inspect/issues/1))

## 0.1.5 - 2026-11-15

### Fixed

- `inspectProject` no longer hangs on a function that writes to a map or array parameter
  under a dynamic key inside a loop with branches. The callable-value solver kept adding
  members to a value that had no inputs yet, so its states alternated forever instead of
  converging.

### Changed

- The callable-value solver is bounded per function. If it ever fails to converge,
  `inspectProject` rejects with an error naming the function instead of spinning.

## 0.1.4 - 2026-10-22

### Added

- `inspectProjectSnapshot(input)` returns the analysis as plain data that survives
  structured cloning and JSON, and `projectFromSnapshot(snapshot)` reopens it as a project,
  so one analysis can be shared with worker threads. Snapshots are accepted only by the
  exact analyzer version that produced them.

### Changed

- `references`, `callSites(symbol)`, `calls`, and `callers` look up per-symbol indexes built
  when the project opens instead of scanning every reference or call. Results and their
  order are unchanged, and each call still returns a fresh array.

## 0.1.3 - 2026-10-13

### Changed

- Use stable Acton v1.2.1 (Tolk 1.5.0) with the exact release revision retained in
  `Cargo.lock` and runtime metadata. The Rust toolchain pin moves to 1.98.1 as the new
  crates require.
- Drop the `bytes` type kind: Tolk 1.5 resolves declared `bytesN` aliases as user-defined
  types and uses `bitsN` for fixed-size bit strings.

### Added

- Upgrade regressions for loop `break`/`continue` control flow, nullable smart casts, and
  declared `bytesN` aliases of `bitsN`.

## 0.1.2 - 2026-09-17

### Changed

- Use stable Acton v1.2.0 instead of an unreleased commit, with the exact release revision
  retained in `Cargo.lock` and runtime metadata.

### Added

- Upgrade regressions for direct match-arm statements, their control flow and callback
  targets, parser fixes, and the new `S009` (`prefer grams`) diagnostic and fix behavior.

## 0.1.1 - 2026-09-16

### Fixed

- Name Andrei Karavatski as the copyright holder in the MIT license and add a
  repository-root license matching the npm package's copy.

## 0.1.0 - 2026-09-15

### Added

- Immutable, in-memory Tolk project inspection through Node.js and WebAssembly.
- Typed AST traversal, symbols, references, resolution, types, and semantic access facts.
- Exact constant evaluation for constants and enum members.
- Per-callable control-flow graphs with traversal, reachability, and dominance helpers.
- Direct and conservative indirect call analysis across functions, lambdas, control flow,
  returns, recursion, captures, destructuring, and standard collection operations.
- Parser, project, resolution, type, and Acton linter diagnostics with structured fixes.
- Compatibility fixtures and clean-consumer package tests against the pinned Acton backend.
