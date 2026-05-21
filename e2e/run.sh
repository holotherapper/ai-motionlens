#!/usr/bin/env bash
# Run the full E2E test suite against the latest release binary.
# Usage:
#   ./e2e/run.sh                # run everything
#   ./e2e/run.sh test_full_e2e  # run a specific test by name (without .py)

set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "${HERE}/.." && pwd)"

if [[ ! -x "${REPO}/target/release/ai-motionlens-mcp" ]]; then
  echo "==> Building release MCP server..."
  (cd "${REPO}" && cargo build --release -p ai-motionlens-mcp)
fi

cd "${HERE}"

FILTER="${1:-}"
fail=0
for f in tests/test_*.py; do
  name="$(basename "${f}" .py)"
  if [[ -n "${FILTER}" && "${name}" != "${FILTER}" ]]; then continue; fi
  echo "==> ${name}"
  if ! uv run --no-project python "${f}"; then
    echo "FAIL: ${name}"
    fail=1
  fi
done

if [[ "${fail}" -eq 0 ]]; then
  echo "All E2E tests passed."
else
  exit 1
fi
