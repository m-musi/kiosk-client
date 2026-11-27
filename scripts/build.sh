#!/usr/bin/env bash
# ============================================================
# Kiosk client environment packaging script (macOS / Linux)
# Usage:
#   scripts/build.sh          -> prod package (default)
#   scripts/build.sh test     -> test package
#   scripts/build.sh dev      -> dev package
#   scripts/build.sh prod --debug -> debug build for quicker test
# Extra args are forwarded to "cargo tauri build".
#
# macOS note:
#   The bundle is ad-hoc signed (codesign --sign -) after Tauri builds the .app,
#   then the .dmg is regenerated with hdiutil. This avoids the "已损坏，无法打开"
#   error on other Macs. Ad-hoc signing does NOT pass Gatekeeper; the recipient
#   must right-click -> Open (or run `xattr -cr` on the .app). For double-click
#   install, a Developer ID certificate + notarization is required.
#
#   Updater artifacts are rebuilt from the signed .app and named
#   <App>_<version>_<arch>.app.tar.gz (+ .sig), ready to paste into latest.json.
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

MACOS_BUNDLE_DIR="src-tauri/target/release/bundle/macos"
DMG_DIR="src-tauri/target/release/bundle/dmg"

if [ "$(uname -s)" = "Darwin" ]; then
  # Clear old bundles so a failed build cannot be mistaken for a fresh one.
  rm -rf "$MACOS_BUNDLE_DIR"/*.app
  rm -f "$MACOS_BUNDLE_DIR"/*.tar.gz "$MACOS_BUNDLE_DIR"/*.sig
  rm -f "$DMG_DIR"/*.dmg
fi

# Tauri's bundle_dmg.sh can fail at its mount check on some macOS versions even
# though the .app was built successfully. Allow a non-zero exit and finish the
# dmg ourselves below; a genuinely failed build is caught by the missing .app.
set +e
cargo tauri build --config "$CONF" "$@"
BUILD_RC=$?
set -e

if [ "$(uname -s)" != "Darwin" ]; then
  exit "$BUILD_RC"
fi

APP="$(ls -dt "$MACOS_BUNDLE_DIR"/*.app 2>/dev/null | head -n 1 || true)"
if [ -z "$APP" ]; then
  echo "[error] no .app bundle produced (build failed, rc=$BUILD_RC)" >&2
  exit 1
fi
APP_NAME="$(basename "$APP" .app)"

# Ad-hoc sign the whole bundle. Without this, macOS reports the app as damaged
# ("已损坏，无法打开") on machines that never built it. `--deep` also signs any
# nested helpers/frameworks, and seals Info.plist + resources.
echo "Ad-hoc signing $APP_NAME.app ..."
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP" 2>/dev/null || true

mkdir -p "$DMG_DIR"
case "$(uname -m)" in
  arm64) ARCH="aarch64" ;;
  x86_64) ARCH="x64" ;;
  *) ARCH="$(uname -m)" ;;
esac
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist" 2>/dev/null || echo 0.1.0)"

# Rebuild the updater artifact from the FULLY SIGNED app, then re-sign it.
# Tauri generates <App>.app.tar.gz BEFORE our ad-hoc codesign, so its payload
# is only linker-signed (Sealed Resources=none) and shows "已损坏" on other Macs.
# Re-tar here seals the signed app, and re-sign keeps the .sig paired with it.
# Name it <App>_<version>_<arch>.app.tar.gz (same convention as the dmg / Windows).
if [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
  NEW_TGZ="$MACOS_BUNDLE_DIR/${APP_NAME}_${VERSION}_${ARCH}.app.tar.gz"
  echo "Building updater artifact from signed app: $(basename "$NEW_TGZ")"
  (cd "$MACOS_BUNDLE_DIR" && tar -czf "$(basename "$NEW_TGZ")" "$(basename "$APP")")
  cargo tauri signer sign --app-version "$VERSION" "$NEW_TGZ"
  echo "Updater artifact signed: $(basename "$NEW_TGZ") + .sig"
else
  echo "[warn] no private key; updater artifact not created" >&2
fi

# Regenerate the dmg from the signed bundle.
DMG="$DMG_DIR/${APP_NAME}_${VERSION}_${ARCH}.dmg"

# Detach any stale read-write image from a previous failed Tauri dmg step.
for dev in $(hdiutil info | awk '/image-path.*Elecbalance/{found=1} found && /\/dev\/disk/{print $1; found=0}'); do
  hdiutil detach "$dev" -force >/dev/null 2>&1 || true
done

echo "Creating dmg: $DMG"
hdiutil create -volname "$APP_NAME" -srcfolder "$APP" -ov -format UDZO "$DMG"
hdiutil verify "$DMG" | tail -n 2

echo "Done: $DMG"
echo "Recipient: right-click the .app -> Open, or run: xattr -cr <path-to-.app>"
echo "Updater artifact: $MACOS_BUNDLE_DIR/${APP_NAME}_${VERSION}_${ARCH}.app.tar.gz (+ .sig)"
