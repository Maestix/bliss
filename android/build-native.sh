#!/usr/bin/env bash
# Build what the APK carries natively, into app/src/main/jniLibs/arm64-v8a:
#   libbliss.so — the tunnel's engine (crates/android), Rust;
#   libxray.so  — xray-core, an executable Android runs from the folder it
#                 unpacks native libraries into (see packaging in app/build.gradle.kts).
#
# Needs: ANDROID_NDK_HOME (NDK r28 or later: 16 KB pages by default),
# rustup's aarch64-linux-android target, and Go (which fetches its own
# toolchain when xray asks for a newer one).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
ndk="${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to the NDK}"
api=29
xray_version="${XRAY_VERSION:-v1.260327.0}"   # xray 26.3.27, the one the engine test runs
bin="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin"
out="$here/app/src/main/jniLibs/arm64-v8a"
mkdir -p "$out"

export CC_aarch64_linux_android="$bin/aarch64-linux-android$api-clang"
export AR_aarch64_linux_android="$bin/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$bin/aarch64-linux-android$api-clang"
(cd "$repo" && cargo build --release -p bliss-android --target aarch64-linux-android)
cp "$repo/target/aarch64-linux-android/release/libbliss.so" "$out/libbliss.so"

# cgo, so names resolve through Android's own resolver rather than a
# /etc/resolv.conf the phone does not have.
gobin="$(mktemp -d)"
(
    cd "$gobin"
    GOTOOLCHAIN=auto GOFLAGS=-mod=mod CGO_ENABLED=1 GOOS=android GOARCH=arm64 \
        CC="$bin/aarch64-linux-android$api-clang" GOPATH="$gobin/go" \
        go install -trimpath -ldflags="-s -w -buildid=" "github.com/xtls/xray-core/main@$xray_version"
)
cp "$gobin/go/bin/android_arm64/main" "$out/libxray.so"
chmod -R u+w "$gobin" && rm -rf "$gobin"

# Google Play asks for 16 KB pages of every native library (docs/android-stack.md).
for lib in "$out"/*.so; do
    if "$bin/llvm-readelf" -l "$lib" | awk '/LOAD/ {print $NF}' | grep -qv 0x4000; then
        echo "$lib is not aligned to 16 KB pages" >&2
        exit 1
    fi
done
ls -la "$out"
