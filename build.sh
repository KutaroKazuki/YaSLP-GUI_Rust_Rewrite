#!/bin/bash
# YaSLP-GUI-Rust build script
# Run this once to install dependencies and build the project.

set -e

echo "==> Installing build dependencies..."
sudo apt-get install -y \
    build-essential \
    pkg-config \
    libssl-dev \
    libwayland-dev \
    libxkbcommon-dev \
    libxcb-render0-dev \
    libxcb-shape0-dev \
    libxcb-xfixes0-dev \
    libxcb-xkb-dev \
    libx11-dev \
    libxcursor-dev \
    libxi-dev \
    libxrandr-dev

echo "==> Removing temporary linker workarounds..."
rm -f /home/kutaro/.local/lib/libgcc_s.so
rm -f /home/kutaro/.local/lib/libc.so
rm -f /home/kutaro/.local/lib/libm.so
rm -f /home/kutaro/.local/lib/libdl.so
rm -f /home/kutaro/.local/lib/libpthread.so
rm -f /home/kutaro/.local/lib/librt.so
rm -f /home/kutaro/.local/lib/libutil.so

echo "==> Restoring default cargo config..."
cat > .cargo/config.toml << 'EOF'
# Default config - uses system gcc linker after build-essential is installed
EOF

echo "==> Building YaSLP-GUI (this may take a few minutes)..."
source "$HOME/.cargo/env"
cargo build --release

echo ""
echo "==> Done! Binary is at: target/release/yaslp-gui"
echo "    Run with: ./target/release/yaslp-gui"
