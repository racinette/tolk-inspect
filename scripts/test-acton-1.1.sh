#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
acton_bin="${ACTON_V1_1_BIN:-acton}"
version="$(${acton_bin} --version)"
if [[ "${version}" != *"1.1.0"* ]]; then
  echo "Expected Acton 1.1.0, got: ${version}" >&2
  exit 1
fi

compat_dir="$(mktemp -d)"
trap 'rm -rf "${compat_dir}"' EXIT
cp -R "${repo_root}/fixtures/upstream/acton-v1.1.0/tests/projects/basic/." "${compat_dir}/"
(
  cd "${compat_dir}"
  "${acton_bin}" build --out-dir "${compat_dir}/build"
)
