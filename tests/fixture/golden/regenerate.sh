#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
exec cargo run --features fixture-tools --manifest-path "$repo_root/Cargo.toml" -- \
  --project-root "$repo_root" \
  fixture golden regenerate "$@"
