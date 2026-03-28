#!/bin/bash
# YaSLP-GUI-Rust build script
# Builds yaslp-gui and yaslp-web for:
#   - Linux x86_64  (x86_64-unknown-linux-gnu)
#   - Linux ARMv7   (armv7-unknown-linux-gnueabihf)
#   - Linux AArch64 (aarch64-unknown-linux-gnu)
#   - Windows x86_64 (x86_64-pc-windows-gnu)

set -e

TARGETS=(
    "x86_64-unknown-linux-gnu"
    "armv7-unknown-linux-gnueabihf"
    "aarch64-unknown-linux-gnu"
    "x86_64-pc-windows-gnu"
)

PACKAGES=(
    "yaslp-gui"
    "yaslp-web"
)

source "$HOME/.cargo/env"

echo "==> Installing cross-compilation toolchains..."
sudo apt-get install -y \
    build-essential \
    pkg-config \
    libssl-dev \
    gcc-arm-linux-gnueabihf \
    gcc-aarch64-linux-gnu \
    mingw-w64

echo ""
echo "==> Adding Rust targets..."
for TARGET in "${TARGETS[@]}"; do
    rustup target add "$TARGET"
done

echo ""
echo "==> Building all packages for all targets..."
for TARGET in "${TARGETS[@]}"; do
    for PKG in "${PACKAGES[@]}"; do
        echo ""
        echo "--- Building $PKG for $TARGET ---"

        if [[ "$TARGET" == "x86_64-unknown-linux-gnu" || "$TARGET" == "x86_64-pc-windows-gnu" ]]; then
            cargo build --release --target "$TARGET" --package "$PKG"

        elif [[ "$TARGET" == "armv7-unknown-linux-gnueabihf" ]]; then
            PKG_CONFIG_ALLOW_CROSS=1 \
            PKG_CONFIG_SYSROOT_DIR=/ \
            CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc \
            CXX_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-g++ \
            AR_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-ar \
            cargo build --release --target "$TARGET" --package "$PKG"

        elif [[ "$TARGET" == "aarch64-unknown-linux-gnu" ]]; then
            PKG_CONFIG_ALLOW_CROSS=1 \
            PKG_CONFIG_SYSROOT_DIR=/ \
            CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
            CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
            AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
            cargo build --release --target "$TARGET" --package "$PKG"
        fi
    done
done

echo ""
echo "==> Build complete! Outputs:"
for TARGET in "${TARGETS[@]}"; do
    EXT=""
    [[ "$TARGET" == "x86_64-pc-windows-gnu" ]] && EXT=".exe"
    for PKG in "${PACKAGES[@]}"; do
        OUT="target/$TARGET/release/$PKG$EXT"
        if [ -f "$OUT" ]; then
            SIZE=$(du -sh "$OUT" | cut -f1)
            echo "  [$SIZE] $OUT"
        else
            echo "  [MISSING] $OUT"
        fi
    done
done
