@echo off
REM ============================================================
REM Kiosk client environment packaging script
REM Usage:
REM   scripts\build.cmd          -> prod package (default)
REM   scripts\build.cmd test     -> test package
REM   scripts\build.cmd dev      -> dev package
REM   scripts\build.cmd prod --debug -> debug build for quicker test
REM Extra args are forwarded to "cargo tauri build".
REM ============================================================
setlocal enabledelayedexpansion

set ENV=%~1
if "%ENV%"=="" set ENV=prod

set "KIOSK_ENV=%ENV%"
if /I "%ENV%"=="dev"  set "CONF=src-tauri\tauri.dev.conf.json"
if /I "%ENV%"=="test" set "CONF=src-tauri\tauri.test.conf.json"
if /I "%ENV%"=="prod" set "CONF=src-tauri\tauri.prod.conf.json"

if "%CONF%"=="" (
  echo Unknown environment: %ENV% ^(use dev / test / prod^)
  exit /b 1
)

REM Updater signing key (unencrypted). Generate once with:
REM   cargo tauri signer generate -w .keys/kiosk.key --ci
if exist .keys\kiosk.key (
  REM .keys\kiosk.key is a SINGLE-LINE base64 of the minisign secret key.
  REM Tauri v2 base64-decodes TAURI_SIGNING_PRIVATE_KEY, so pass the whole line.
  for /f "delims=" %%A in (.keys\kiosk.key) do set "TAURI_SIGNING_PRIVATE_KEY=%%A"
  set "TAURI_SIGNING_PRIVATE_KEY_PASSWORD="
) else (
  echo [warn] .keys\kiosk.key not found, updater artifacts will fail.
  echo        Run: cargo tauri signer generate -w .keys/kiosk.key --ci
)

echo Building "%ENV%" package: KIOSK_ENV=%KIOSK_ENV%, config=%CONF%
cargo tauri build --config "%CONF%" %2 %3 %4 %5

endlocal
