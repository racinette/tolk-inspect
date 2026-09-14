#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo_bin="${CARGO:-cargo}"
bindgen_bin="${WASM_BINDGEN:-wasm-bindgen}"

if [[ -n "${WASI_SDK_PATH:-}" ]]; then
  export CC_wasm32_unknown_unknown="${WASI_SDK_PATH}/bin/clang"
fi
export CFLAGS_wasm32_unknown_unknown="${CFLAGS_wasm32_unknown_unknown:--Wno-incompatible-pointer-types}"

"${cargo_bin}" build --manifest-path "${repo_root}/Cargo.toml" --release --target wasm32-unknown-unknown -p tolk-inspect-wasm
mkdir -p "${repo_root}/packages/tolk-inspect/generated"
"${bindgen_bin}" \
  --target nodejs \
  --out-dir "${repo_root}/packages/tolk-inspect/generated" \
  --out-name tolk_inspect_wasm \
  "${repo_root}/target/wasm32-unknown-unknown/release/tolk_inspect_wasm.wasm"
mv -f \
  "${repo_root}/packages/tolk-inspect/generated/tolk_inspect_wasm.js" \
  "${repo_root}/packages/tolk-inspect/generated/tolk_inspect_wasm.cjs"

