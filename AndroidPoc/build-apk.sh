#!/usr/bin/env bash
# Cross-compiles Mailstream's Android PoC (`aarch64-linux-android`) and packages a debug-signed
# arm64-v8a APK using only Android NDK + SDK build-tools (no Gradle required).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
POC="$ROOT/AndroidPoc"
PROFILE="${1:-release}"

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

# Keep cargo build output outside ovnfs checkouts.
if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  if [[ "$ROOT" == "$HOME/checkouts/"* ]]; then
    CHECKOUT_NAME="$(basename "$ROOT")"
    export CARGO_TARGET_DIR="$HOME/.cache/mailstream-checkouts/$CHECKOUT_NAME/android-poc-target"
  else
    export CARGO_TARGET_DIR="$POC/target"
  fi
fi

CARGO_FLAGS=(--manifest-path "$POC/Cargo.toml" --target aarch64-linux-android --lib)
if [[ "$PROFILE" == "release" ]]; then
  CARGO_FLAGS+=(--release)
fi

echo "==> Building libmailstream_android_poc.so ($PROFILE, aarch64-linux-android)"
"$ROOT/scripts/capped" cargo build "${CARGO_FLAGS[@]}"

SO="$CARGO_TARGET_DIR/aarch64-linux-android/$PROFILE/libmailstream_android_poc.so"
[[ -f "$SO" ]] || { echo "error: missing $SO" >&2; exit 1; }

OUT_DIR="$CARGO_TARGET_DIR/apk-$PROFILE"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR/classes" "$OUT_DIR/stage/lib/arm64-v8a"

echo "==> Compiling Java host (GpuiInputActivity + GpuiClipboard)"
javac --release 11 -classpath "$ANDROID_JAR" -d "$OUT_DIR/classes" \
  "$POC/android/src/dev/gpui/mobile/GpuiInputActivity.java" \
  "$POC/android/src/dev/gpui/mobile/GpuiClipboard.java"

echo "==> Dexing classes with d8"
"$BUILD_TOOLS/d8" --min-api "$API_LEVEL" --lib "$ANDROID_JAR" --output "$OUT_DIR/stage" \
  $(find "$OUT_DIR/classes" -name '*.class')

echo "==> Linking AndroidManifest.xml with aapt2"
"$BUILD_TOOLS/aapt2" link \
  -I "$ANDROID_JAR" \
  --manifest "$POC/android/AndroidManifest.xml" \
  --min-sdk-version "$API_LEVEL" \
  --target-sdk-version 35 \
  -o "$OUT_DIR/unaligned.apk"

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
            # Keep .so deflated (extractNativeLibs=true) for smaller APK size.
            zf.write(p, arc, compress_type=zipfile.ZIP_DEFLATED)
PY

echo "==> Aligning APK with zipalign"
"$BUILD_TOOLS/zipalign" -f -p 4 "$OUT_DIR/unaligned.apk" "$OUT_DIR/aligned.apk"

KEYSTORE="${ANDROID_DEBUG_KEYSTORE:-$HOME/.android/debug.keystore}"
if [[ ! -f "$KEYSTORE" ]]; then
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
echo "==> Signing APK with apksigner"
"$BUILD_TOOLS/apksigner" sign \
  --ks "$KEYSTORE" \
  --ks-pass pass:android \
  --key-pass pass:android \
  --ks-key-alias androiddebugkey \
  --out "$FINAL_APK" \
  "$OUT_DIR/aligned.apk"

echo "==> Verifying APK"
"$BUILD_TOOLS/apksigner" verify --verbose "$FINAL_APK"
"$BUILD_TOOLS/zipalign" -c -v 4 "$FINAL_APK" | tail -n 5
"$BUILD_TOOLS/aapt2" dump badging "$FINAL_APK" | head -n 12
ls -lh "$SO" "$FINAL_APK"
