#!/usr/bin/env bash
# Build the i386 game runtime: a Debian bookworm i386 root with Xvfb, the X
# client libraries LWJGL 2 needs, gl4es (desktop GL on the SGX545's GLES2),
# a 32-bit Java 8 and the launcher scripts. Output: runtime-i386.tar.xz.
#
# Why i386 at all on an x86_64 box: the only SGX545 userspace that exists is
# Intel's 32-bit Cedarview DDK 1.7, so any process that draws with the GPU is a
# 32-bit process (openHC's sgx545ce kernel driver translates its bridge calls).
# The game, gl4es and the JVM therefore all run in this i386 root under IA32
# emulation; the box's own userspace stays 64-bit.
#
# Contains no Mojang files: the game is fetched on the box by mc-install from
# Mojang's servers, using the file list made here (mcfiles.py). The DDK blobs are
# not in here either — they are proprietary and already on every openHC EA image
# (/opt/sgx-i386*); mc-session copies them into the root when a session starts.
#
# Needs root (debootstrap, chroot). Runs on an x86_64 Linux host, which executes
# the i386 root natively — no qemu.
#
#   build-runtime.sh OUT_DIR
set -euo pipefail

OUT="$(realpath "${1:?usage: build-runtime.sh OUT_DIR}")"
HERE="$(cd "$(dirname "$0")" && pwd)"
SUITE=bookworm
MIRROR=http://deb.debian.org/debian
MC_VERSION=1.7.10
# Zulu is the maintained OpenJDK 8 build that still ships linux i686.
JRE_URL=https://cdn.azul.com/zulu/bin/zulu8.98.0.107-ca-jre8.0.512-linux_i686.tar.gz
GL4ES_REPO=https://github.com/ptitSeb/gl4es.git
GL4ES_REF=2037865590d410bcd7b8d1f22b841472ed6d39ac

R="$OUT/root"
rm -rf "$R"
mkdir -p "$OUT"

# Runtime packages. Xvfb is the display (input arrives through XTest from
# mc-inputd); the X libs are what LWJGL 2.9's liblwjgl.so and the JRE's AWT link.
PKGS=(
  xvfb xauth x11-xkb-utils xkb-data
  libx11-6 libxext6 libxcursor1 libxrandr2 libxxf86vm1 libxrender1 libxtst6 libxi6 libxinerama1
  libfreetype6 libfontconfig1 fonts-dejavu-core libasound2 libdrm2
  ca-certificates wget unzip procps e2fsprogs
)
debootstrap --arch=i386 --variant=minbase --include="$(IFS=,; echo "${PKGS[*]}")" \
  "$SUITE" "$R" "$MIRROR"

# --- gl4es: build in the root against its own headers, then drop the toolchain ---
git clone -q "$GL4ES_REPO" "$R/tmp/gl4es"
git -C "$R/tmp/gl4es" checkout -q "$GL4ES_REF"
for p in "$HERE"/gl4es/*.patch; do
  patch -d "$R/tmp/gl4es" -p1 --batch < "$p"
done
mount --bind /proc "$R/proc"
trap 'umount "$R/proc" 2>/dev/null || true' EXIT
cp /etc/resolv.conf "$R/etc/resolv.conf"
BUILD_DEPS="build-essential cmake libx11-dev"
chroot "$R" sh -ec "
  apt-get update -q
  DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends $BUILD_DEPS
  cd /tmp/gl4es && mkdir -p build && cd build
  # -march=bonnell: the EA's Atom CE5310 (SSSE3, MOVBE). -DNO_GBM: no GBM here.
  cmake .. -DCMAKE_BUILD_TYPE=RelWithDebInfo -DNO_GBM=ON -DUSE_CLOCK=ON \
        -DCMAKE_C_FLAGS='-march=bonnell -mtune=bonnell'
  make -j\$(nproc)
  install -D -m 0755 ../lib/libGL.so.1 /usr/lib/gl4es/libGL.so.1
  DEBIAN_FRONTEND=noninteractive apt-get purge -y -q --auto-remove $BUILD_DEPS
  apt-get clean
  rm -rf /tmp/gl4es /var/lib/apt/lists/*
"
umount "$R/proc"; trap - EXIT

# --- Java 8, 32-bit ---
mkdir -p "$R/opt/java"
wget -q -O- "$JRE_URL" | tar xz -C "$R/opt/java"
ln -sfn "$(basename "$JRE_URL" .tar.gz)" "$R/opt/java/jre8"

# --- the game's file list (URLs only) + launcher scripts ---
mkdir -p "$R/opt/minecraft"
( cd "$R/opt/minecraft" && python3 "$HERE/mcfiles.py" "$MC_VERSION" )
install -m 0755 "$HERE"/bin/* "$R/usr/local/bin/"
# Where mc-session puts the DDK's EGL/GLES at session start.
mkdir -p "$R/usr/lib/sgx"

# Leave no host state behind.
rm -f "$R/etc/resolv.conf"
echo "openhc-minecraft runtime, $SUITE i386, minecraft $MC_VERSION, built $(date -u +%FT%TZ)" \
  > "$R/etc/openhc-minecraft-runtime"

tar -C "$R" -cf - . | xz -T0 -6 > "$OUT/runtime-i386.tar.xz"
ls -l "$OUT/runtime-i386.tar.xz"
