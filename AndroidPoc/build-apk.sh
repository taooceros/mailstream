#!/usr/bin/env bash
# Cross-compiles Mailstream's Android PoC (`aarch64-linux-android`) and packages a signed
# arm64-v8a APK using only Android NDK + SDK build-tools (no Gradle required).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
POC="$ROOT/AndroidPoc"
PROFILE="${1:-release}"

# Resolve signing config up front so missing keystore fails before a long cargo build.
# Default: shared out-of-repo keystore (~/.config/mailstream/android-signing.{jks,env}, chmod 600)
# so Mac and Linux produce identical APK signatures. Set ANDROID_USE_DEBUG_KEYSTORE=1 to opt in
# to a local-only debug keystore (for external contributors or ad-hoc local testing).
USE_DEBUG_KEYSTORE="${ANDROID_USE_DEBUG_KEYSTORE:-0}"
if [[ "$USE_DEBUG_KEYSTORE" == "1" ]]; then
  KEYSTORE="${ANDROID_DEBUG_KEYSTORE:-$HOME/.android/debug.keystore}"
  KEY_ALIAS="androiddebugkey"
  KEYSTORE_PASS="android"
  KEY_PASS="android"
else
  _env_ks="${ANDROID_KEYSTORE:-}"
  _env_alias="${ANDROID_KEY_ALIAS:-}"
  _env_ks_pass="${ANDROID_KEYSTORE_PASS:-}"
  _env_key_pass="${ANDROID_KEY_PASS:-}"
  SIGNING_DIR="${MAILSTREAM_ANDROID_SIGNING_DIR:-$HOME/.config/mailstream}"
  SIGNING_ENV="${ANDROID_SIGNING_ENV:-$SIGNING_DIR/android-signing.env}"
  if [[ -f "$SIGNING_ENV" ]]; then
    # shellcheck disable=SC1090
    source "$SIGNING_ENV"
  fi
  KEYSTORE="${_env_ks:-${ANDROID_KEYSTORE:-$SIGNING_DIR/android-signing.jks}}"
  KEY_ALIAS="${_env_alias:-${ANDROID_KEY_ALIAS:-mailstream-android}}"
  KEYSTORE_PASS="${_env_ks_pass:-${ANDROID_KEYSTORE_PASS:-}}"
  KEY_PASS="${_env_key_pass:-${ANDROID_KEY_PASS:-$KEYSTORE_PASS}}"
  if [[ ! -f "$KEYSTORE" || -z "$KEYSTORE_PASS" || -z "$KEY_ALIAS" ]]; then
    cat >&2 <<EOF
error: missing shared Android signing configuration.
  expected keystore: $KEYSTORE
  expected env file: $SIGNING_ENV (defining ANDROID_KEYSTORE_PASS, ANDROID_KEY_PASS, ANDROID_KEY_ALIAS)

Sync the shared keystore from Linear onto this machine:
  python3 AndroidPoc/sync-signing-key.py --doc-id <LINEAR_DOC_ID>

Or, for a local-only build without the shared team key (incompatible with cross-machine updates):
  ANDROID_USE_DEBUG_KEYSTORE=1 AndroidPoc/build-apk.sh $PROFILE
EOF
    exit 1
  fi
fi

sdk_roots() {
  for dir in \
    "${ANDROID_SDK_ROOT:-}" \
    "${ANDROID_HOME:-}" \
    "/opt/homebrew/share/android-commandlinetools" \
    "$HOME/Library/Android/sdk" \
    "$HOME/Android/Sdk"; do
    [[ -n "$dir" && -d "$dir" ]] && echo "$dir"
  done
}

find_ndk() {
  if [[ -n "${ANDROID_NDK_HOME:-}" && -d "$ANDROID_NDK_HOME" ]]; then
    echo "$ANDROID_NDK_HOME"; return
  fi
  local found=""
  while IFS= read -r root; do
    found="$(ls -d "$root"/ndk/* 2>/dev/null | sort -V | tail -n1 || true)"
    [[ -n "$found" ]] && { echo "$found"; return; }
  done < <(sdk_roots)
  found="$(ls -d /nix/store/*-android-sdk-ndk-*/libexec/android-sdk/ndk/* 2>/dev/null | sort -V | tail -n1 || true)"
  [[ -n "$found" ]] || { echo "error: ANDROID_NDK_HOME not set and no Android NDK found" >&2; exit 1; }
  echo "$found"
}

find_build_tools() {
  if [[ -n "${ANDROID_BUILD_TOOLS:-}" && -d "$ANDROID_BUILD_TOOLS" ]]; then
    echo "$ANDROID_BUILD_TOOLS"; return
  fi
  local found=""
  while IFS= read -r root; do
    found="$(ls -d "$root"/build-tools/* 2>/dev/null | sort -V | tail -n1 || true)"
    [[ -n "$found" ]] && { echo "$found"; return; }
  done < <(sdk_roots)
  found="$(ls -d /nix/store/*-android-sdk-build-tools-*/libexec/android-sdk/build-tools/* 2>/dev/null | sort -V | tail -n1 || true)"
  [[ -n "$found" ]] || { echo "error: ANDROID_BUILD_TOOLS not set and no build-tools found" >&2; exit 1; }
  echo "$found"
}

find_android_jar() {
  if [[ -n "${ANDROID_PLATFORM_JAR:-}" && -f "$ANDROID_PLATFORM_JAR" ]]; then
    echo "$ANDROID_PLATFORM_JAR"; return
  fi
  local found=""
  while IFS= read -r root; do
    found="$(ls "$root"/platforms/android-*/android.jar 2>/dev/null | sort -V | tail -n1 || true)"
    [[ -n "$found" ]] && { echo "$found"; return; }
  done < <(sdk_roots)
  found="$(ls /nix/store/*-android-sdk-platforms-*/libexec/android-sdk/platforms/android-*/android.jar 2>/dev/null | sort -V | tail -n1 || true)"
  [[ -n "$found" ]] || { echo "error: ANDROID_PLATFORM_JAR not set and no android.jar found" >&2; exit 1; }
  echo "$found"
}

NDK="$(find_ndk)"
BUILD_TOOLS="$(find_build_tools)"
ANDROID_JAR="$(find_android_jar)"
if [[ "$(uname -s)" == "Darwin" ]]; then
  HOST_TAG="darwin-x86_64"
else
  HOST_TAG="linux-x86_64"
fi
TOOLCHAIN="$NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin"
API_LEVEL=26
export CC_aarch64_linux_android="$TOOLCHAIN/aarch64-linux-android${API_LEVEL}-clang"
export CXX_aarch64_linux_android="$TOOLCHAIN/aarch64-linux-android${API_LEVEL}-clang++"
export AR_aarch64_linux_android="$TOOLCHAIN/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CC_aarch64_linux_android"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_AR="$AR_aarch64_linux_android"
# Android 15+ 16 KB ELF page alignment + linker dead-code section GC (MS-441).
ANDROID_RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384 -C link-arg=-Wl,--gc-sections"
if [[ -n "${CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS:-}" ]]; then
  export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="$CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS $ANDROID_RUSTFLAGS"
else
  export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="$ANDROID_RUSTFLAGS"
fi

# Keep cargo build output outside ovnfs checkouts.
if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  if [[ "$ROOT" == "$HOME/checkouts/"* ]]; then
    CHECKOUT_NAME="$(basename "$ROOT")"
    export CARGO_TARGET_DIR="$HOME/.cache/mailstream-checkouts/$CHECKOUT_NAME/android-poc-target"
  else
    export CARGO_TARGET_DIR="$POC/target"
  fi
fi

CARGO_FLAGS=(--manifest-path "$ROOT/Cargo.toml" -p mailstream-android-poc --target aarch64-linux-android --lib)
if [[ "$PROFILE" == "release" ]]; then
  CARGO_FLAGS+=(--profile android-release)
  TARGET_PROFILE="android-release"
else
  TARGET_PROFILE="$PROFILE"
fi

echo "==> Building libmailstream_android_poc.so ($PROFILE, aarch64-linux-android)"
"$ROOT/scripts/capped" cargo build "${CARGO_FLAGS[@]}"

SO="$CARGO_TARGET_DIR/aarch64-linux-android/$TARGET_PROFILE/libmailstream_android_poc.so"
[[ -f "$SO" ]] || { echo "error: missing $SO" >&2; exit 1; }

OUT_DIR="$CARGO_TARGET_DIR/apk-$PROFILE"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR/classes" "$OUT_DIR/stage/lib/arm64-v8a"

echo "==> Compiling Java host (GpuiInputActivity + GpuiClipboard)"
javac --release 11 -classpath "$ANDROID_JAR" -d "$OUT_DIR/classes" \
  "$POC/android/src/dev/gpui/mobile/GpuiInputActivity.java" \
  "$POC/android/src/dev/gpui/mobile/GpuiClipboard.java"

echo "==> Dexing classes with d8"
D8_FLAGS=(--min-api "$API_LEVEL" --lib "$ANDROID_JAR" --output "$OUT_DIR/stage")
if [[ "$PROFILE" == "release" ]]; then
  D8_FLAGS+=(--release)
fi
"$BUILD_TOOLS/d8" "${D8_FLAGS[@]}" \
  $(find "$OUT_DIR/classes" -name '*.class')

echo "==> Compiling Android resources with aapt2"
mkdir -p "$OUT_DIR/res"
"$BUILD_TOOLS/aapt2" compile --dir "$POC/android/res" -o "$OUT_DIR/res/resources.zip"

echo "==> Linking AndroidManifest.xml and resources with aapt2"
"$BUILD_TOOLS/aapt2" link \
  -I "$ANDROID_JAR" \
  --manifest "$POC/android/AndroidManifest.xml" \
  --min-sdk-version "$API_LEVEL" \
  --target-sdk-version 35 \
  -o "$OUT_DIR/unaligned.apk" \
  "$OUT_DIR/res/resources.zip"

cp "$SO" "$OUT_DIR/stage/lib/arm64-v8a/libmailstream_android_poc.so"

echo "==> Packing classes.dex and lib/arm64-v8a/libmailstream_android_poc.so"
python3 - "$OUT_DIR/unaligned.apk" "$OUT_DIR/stage" <<'PY'
import pathlib, sys, zipfile

apk_path = pathlib.Path(sys.argv[1])
stage = pathlib.Path(sys.argv[2])
with zipfile.ZipFile(apk_path, "a", compression=zipfile.ZIP_DEFLATED) as zf:
    for p in sorted(stage.rglob("*")):
        if p.is_file():
            arc = p.relative_to(stage).as_posix()
            # Store native .so uncompressed (extractNativeLibs=false) for direct 16 KB page-aligned mmap.
            compression = zipfile.ZIP_STORED if arc.endswith(".so") else zipfile.ZIP_DEFLATED
            zf.write(p, arc, compress_type=compression)
PY

echo "==> Aligning APK with zipalign (16 KB page alignment for .so)"
"$BUILD_TOOLS/zipalign" -f -P 16 4 "$OUT_DIR/unaligned.apk" "$OUT_DIR/aligned.apk"

if [[ "$USE_DEBUG_KEYSTORE" == "1" && ! -f "$KEYSTORE" ]]; then
  mkdir -p "$(dirname "$KEYSTORE")"
  keytool -genkeypair -v \
    -keystore "$KEYSTORE" \
    -storepass android \
    -keypass android \
    -alias androiddebugkey \
    -keyalg RSA \
    -keysize 2048 \
    -validity 10000 \
    -dname "CN=Android Debug,O=Android,C=US" >/dev/null 2>&1
fi

FINAL_APK="$OUT_DIR/mailstream-android-poc-arm64-$PROFILE.apk"
echo "==> Signing APK with apksigner (alias: $KEY_ALIAS, keystore: $KEYSTORE)"
MAILSTREAM_KS_PASS="$KEYSTORE_PASS" MAILSTREAM_KEY_PASS="$KEY_PASS" \
  "$BUILD_TOOLS/apksigner" sign \
    --ks "$KEYSTORE" \
    --ks-pass env:MAILSTREAM_KS_PASS \
    --key-pass env:MAILSTREAM_KEY_PASS \
    --ks-key-alias "$KEY_ALIAS" \
    --out "$FINAL_APK" \
    "$OUT_DIR/aligned.apk"

echo "==> Verifying APK"
"$BUILD_TOOLS/apksigner" verify --verbose --print-certs "$FINAL_APK"
"$BUILD_TOOLS/zipalign" -c -P 16 -v 4 "$FINAL_APK" | tail -n 5
"$TOOLCHAIN/llvm-readelf" -l "$SO" | grep -E 'LOAD' | head -n 6
"$BUILD_TOOLS/aapt2" dump badging "$FINAL_APK" | awk 'NR<=16'
ls -lh "$SO" "$FINAL_APK"
