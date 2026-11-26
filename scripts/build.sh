#!/usr/bin/env bash
# ============================================================
# Kiosk client environment packaging script (macOS / Linux)
# Usage:
#   scripts/build.sh          -> prod package (default)
#   scripts/build.sh test     -> test package
#   scripts/build.sh dev      -> dev package
#   scripts/build.sh prod --debug -> debug build for quicker test
# Extra args are forwarded to "cargo tauri build".
# ============================================================
set -euo pipefail

ENV="${1:-prod}"
case "$ENV" in
  dev)  CONF="src-tauri/tauri.dev.conf.json" ;;
  test) CONF="src-tauri/tauri.test.conf.json" ;;
  prod) CONF="src-tauri/tauri.prod.conf.json" ;;
  *)
    echo "Unknown environment: $ENV (use dev / test / prod)" >&2
    exit 1
    ;;
esac
shift || true

export KIOSK_ENV="$ENV"

# Updater signing key (unencrypted). Generate once with:
#   cargo tauri signer generate -w .keys/kiosk.key --ci
# .keys/kiosk.key is a SINGLE-LINE base64 of the minisign secret key.
# Tauri v2 base64-decodes TAURI_SIGNING_PRIVATE_KEY, so pass the whole line.
if [ -f .keys/kiosk.key ]; then
  export TAURI_SIGNING_PRIVATE_KEY="$(cat .keys/kiosk.key)"
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
else
  echo "[warn] .keys/kiosk.key not found, updater artifacts will fail." >&2
  echo "       Run: cargo tauri signer generate -w .keys/kiosk.key --ci" >&2
fi

echo "Building \"$ENV\" package: KIOSK_ENV=$KIOSK_ENV, config=$CONF"
cargo tauri build --config "$CONF" "$@"
