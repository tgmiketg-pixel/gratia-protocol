#!/usr/bin/env bash
#
# build-android.sh — Cross-compile the Gratia Rust core for Android ARM64
# and generate UniFFI Kotlin bindings.
#
# Usage:
#   ./scripts/build-android.sh          # Release build (default)
#   ./scripts/build-android.sh debug    # Debug build (faster, larger)
#
# Prerequisites:
#   - Rust with aarch64-linux-android target installed
#   - Android NDK 27.x at the path configured in .cargo/config.toml
#   - MinGW toolchain for building the host uniffi-bindgen binary
#
# WHY: The host library build (for bindgen) must run from a path without spaces
# due to a MinGW dlltool bug. The Android cross-compile uses the NDK's clang
# linker which does not have this issue, so it can run from any path.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# WHY: Build profile defaults to release. Debug builds are faster to compile
# but produce much larger .so files (~10x) which is fine during development.
PROFILE="${1:-release}"
if [ "$PROFILE" = "debug" ]; then
    PROFILE_FLAG=""
    TARGET_DIR="debug"
else
    PROFILE_FLAG="--release"
    TARGET_DIR="release"
fi

ANDROID_TARGET="aarch64-linux-android"
JNILIBS_DIR="$PROJECT_ROOT/app/android/src/main/jniLibs/arm64-v8a"
BINDINGS_OUT_DIR="$PROJECT_ROOT/app/android/src/main/kotlin"

# WHY: Space-free path for host builds. MinGW dlltool breaks on paths with
# spaces ("Project GRATIA" triggers this). The Android cross-compile uses
# NDK clang and is not affected.
HOST_BUILD_DIR="/c/Users/Michael/gratia-build"

export PATH="/c/Users/Michael/mingw64/bin:/c/Users/Michael/.cargo/bin:/usr/bin:$PATH"

echo "=== Gratia Android Build ==="
echo "Profile: $PROFILE"
echo "Target:  $ANDROID_TARGET"
echo ""

# ── Step 1: Cross-compile gratia-ffi for Android ARM64 ──────────────────────
echo "[1/4] Cross-compiling gratia-ffi for $ANDROID_TARGET ($PROFILE)..."

cd "$PROJECT_ROOT"
cargo build \
    --target "$ANDROID_TARGET" \
    -p gratia-ffi \
    $PROFILE_FLAG

SO_PATH="$PROJECT_ROOT/target/$ANDROID_TARGET/$TARGET_DIR/libgratia_ffi.so"
if [ ! -f "$SO_PATH" ]; then
    echo "ERROR: Expected .so not found at $SO_PATH"
    exit 1
fi

echo "   Built: $SO_PATH"
echo "   Size:  $(du -h "$SO_PATH" | cut -f1)"

# ── Step 2: Copy .so to Android jniLibs ─────────────────────────────────────
echo "[2/4] Copying .so to jniLibs/arm64-v8a/..."

mkdir -p "$JNILIBS_DIR"
cp "$SO_PATH" "$JNILIBS_DIR/libgratia_ffi.so"

echo "   Copied to: $JNILIBS_DIR/libgratia_ffi.so"

# ── Step 3: Build host library + uniffi-bindgen from space-free path ────────
echo "[3/4] Building uniffi-bindgen (host) from $HOST_BUILD_DIR..."

# Sync source to space-free path
mkdir -p "$HOST_BUILD_DIR"
rm -rf "$HOST_BUILD_DIR/crates" "$HOST_BUILD_DIR/.cargo"
cp -r "$PROJECT_ROOT/crates" "$HOST_BUILD_DIR/crates"
cp -r "$PROJECT_ROOT/.cargo" "$HOST_BUILD_DIR/.cargo"
cp "$PROJECT_ROOT/Cargo.toml" "$HOST_BUILD_DIR/Cargo.toml"
cp "$PROJECT_ROOT/Cargo.lock" "$HOST_BUILD_DIR/Cargo.lock" 2>/dev/null || true

cd "$HOST_BUILD_DIR"
cargo build -p gratia-ffi --lib

# WHY: On Windows with MinGW, the cdylib is named gratia_ffi.dll
HOST_LIB_DIR="$HOST_BUILD_DIR/target/debug"
HOST_LIB=""
for candidate in "$HOST_LIB_DIR/gratia_ffi.dll" "$HOST_LIB_DIR/libgratia_ffi.so" "$HOST_LIB_DIR/libgratia_ffi.dylib"; do
    if [ -f "$candidate" ]; then
        HOST_LIB="$candidate"
        break
    fi
done

if [ -z "$HOST_LIB" ]; then
    echo "ERROR: Could not find host library in $HOST_LIB_DIR"
    ls "$HOST_LIB_DIR"/gratia_ffi* "$HOST_LIB_DIR"/libgratia_ffi* 2>/dev/null || echo "(none)"
    exit 1
fi

echo "   Host library: $HOST_LIB"

# ── Step 4: Generate Kotlin bindings ────────────────────────────────────────
echo "[4/4] Generating Kotlin bindings..."

mkdir -p "$BINDINGS_OUT_DIR"

cargo run -p gratia-ffi --bin uniffi-bindgen -- \
    generate \
    --library "$HOST_LIB" \
    --language kotlin \
    --out-dir "$BINDINGS_OUT_DIR"

echo ""
echo "=== Build complete ==="
echo "Native library: $JNILIBS_DIR/libgratia_ffi.so"
echo "Kotlin bindings: $BINDINGS_OUT_DIR/uniffi/"
echo ""
echo "Generated files:"
find "$BINDINGS_OUT_DIR/uniffi" -name "*.kt" 2>/dev/null | while read f; do
    echo "  $f"
done
