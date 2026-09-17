# Changelog

Notable changes to `tolk-inspect` are documented here. The project follows Semantic
Versioning; while the package is below 1.0, minor releases may change the public API.

## Unreleased

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
