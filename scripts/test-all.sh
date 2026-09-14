#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"
cargo test --workspace
npm run build
npm run test --workspace tolk-inspect
npm run test:e2e

