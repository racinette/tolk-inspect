# Third-party notices

The `tolk-inspect` WebAssembly binary incorporates third-party Rust software. The exact
dependency versions and sources used for each release are recorded in the repository's
committed `Cargo.lock` file.

## Acton and Tolk components

The analyzer incorporates `tolk-analysis`, `tolk-dataflow`, `tolk-linter`, `tolk-resolver`,
`tolk-syntax`, `tolk-ty`, `ton-syntax`, and `tree-sitter-tolk` from:

<https://github.com/ton-blockchain/acton>

Revision: `17654feb713c5824ee4cc0259b7be9b5f72898ba`

Copyright (c) 2025 TON Core

These components are available under the Apache License 2.0 or the MIT License. This
distribution uses them under the MIT option:

> Permission is hereby granted, free of charge, to any person obtaining a copy of this
> software and associated documentation files (the "Software"), to deal in the Software
> without restriction, including without limitation the rights to use, copy, modify,
> merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit
> persons to whom the Software is furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or
> substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
> INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
> PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE
> FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
> OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
> DEALINGS IN THE SOFTWARE.

## Other Rust dependencies

The binary also incorporates the Rust crates in the `tolk-inspect-wasm` dependency tree,
including `wasm-bindgen`, `serde`, `serde-wasm-bindgen`, `tree-sitter`, and their transitive
dependencies. Each remains subject to its own license; the authoritative package names,
versions, sources, and checksums are recorded in `Cargo.lock` at the source revision linked
by npm provenance and the package's `repository` metadata.
