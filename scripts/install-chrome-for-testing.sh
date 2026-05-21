#!/usr/bin/env bash
# Download a Chrome-for-Testing build pinned to the current Stable channel and
# print the binary path. Set CHROME_PATH to that value (or export it) so
# ai-motionlens picks it up.
#
# Usage:
#   ./scripts/install-chrome-for-testing.sh           # mac arm64, stable
#   ./scripts/install-chrome-for-testing.sh --eval    # print `export CHROME_PATH=...` for `eval $(...)`

set -euo pipefail

CACHE_ROOT="${AI_MOTIONLENS_CHROME_CACHE:-${HOME}/.cache/ai-motionlens/chrome-for-testing}"
ENDPOINT="https://googlechromelabs.github.io/chrome-for-testing/known-good-versions-with-downloads.json"

case "$(uname -s)" in
  Darwin) OS="mac" ;;
  Linux)  OS="linux" ;;
  *) echo "unsupported OS: $(uname -s)" >&2; exit 2 ;;
esac
case "$(uname -m)" in
  arm64|aarch64) ARCH="arm64" ;;
  x86_64|amd64)  ARCH="x64" ;;
  *) echo "unsupported arch: $(uname -m)" >&2; exit 2 ;;
esac
PLATFORM="${OS}-${ARCH}"

EVAL_MODE=0
if [[ "${1:-}" == "--eval" ]]; then EVAL_MODE=1; fi

log() { [[ "${EVAL_MODE}" -eq 1 ]] || echo "$@" >&2; }

log "==> Resolving latest Stable Chrome-for-Testing for ${PLATFORM}..."

# We pull the Stable channel, descending order of timestamp, and pick the
# entry whose chrome.downloads includes the target platform.
LATEST_JSON="$(curl -fsSL "${ENDPOINT}")"
ROW="$(printf '%s' "${LATEST_JSON}" \
  | python3 -c '
import json, sys
data = json.load(sys.stdin)
versions = data["versions"]
target = sys.argv[1]
versions.sort(key=lambda v: v.get("revision", "0"), reverse=True)
for v in versions:
    downloads = v.get("downloads", {}).get("chrome", [])
    for d in downloads:
        if d.get("platform") == target:
            print(json.dumps({"version": v["version"], "url": d["url"]}))
            sys.exit(0)
sys.exit("no matching platform in known-good-versions")
' "${PLATFORM}")"

VERSION="$(printf '%s' "${ROW}" | python3 -c 'import json,sys;print(json.load(sys.stdin)["version"])')"
URL="$(printf '%s' "${ROW}" | python3 -c 'import json,sys;print(json.load(sys.stdin)["url"])')"
log "    version: ${VERSION}"
log "    url:     ${URL}"

DEST_DIR="${CACHE_ROOT}/${VERSION}/${PLATFORM}"
mkdir -p "${DEST_DIR}"

case "${PLATFORM}" in
  mac-arm64|mac-x64) BIN_REL="chrome-${PLATFORM}/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing" ;;
  linux-x64)         BIN_REL="chrome-linux64/chrome" ;;
  *) echo "unmapped platform binary path: ${PLATFORM}" >&2; exit 2 ;;
esac
BIN_ABS="${DEST_DIR}/${BIN_REL}"

if [[ -x "${BIN_ABS}" ]]; then
  log "==> Already installed at ${BIN_ABS}"
else
  ZIP="${DEST_DIR}/chrome.zip"
  log "==> Downloading ${URL}"
  curl -fL --progress-bar "${URL}" -o "${ZIP}"
  log "==> Unzipping into ${DEST_DIR}"
  (cd "${DEST_DIR}" && unzip -q -o "${ZIP}")
  rm -f "${ZIP}"
fi

if [[ ! -x "${BIN_ABS}" ]]; then
  echo "post-install: expected binary not executable: ${BIN_ABS}" >&2
  exit 1
fi

if [[ "${EVAL_MODE}" -eq 1 ]]; then
  printf 'export CHROME_PATH=%q\n' "${BIN_ABS}"
else
  echo
  echo "Chrome-for-Testing ${VERSION} installed."
  echo "Binary: ${BIN_ABS}"
  echo
  echo "Set it for ai-motionlens with:"
  printf '  export CHROME_PATH=%q\n' "${BIN_ABS}"
  echo "or:"
  printf '  eval "$(%s --eval)"\n' "$0"
fi
