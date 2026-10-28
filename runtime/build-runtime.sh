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
GL4ES_REF=1891af323d4547487296e50ba5932bde65c7b345
# What runtime/mod compiles against (the box fetches the same jars with the game).
LW_URL=https://libraries.minecraft.net/net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar
LW_SHA1=111e7bea9c968cdb3d06ef4632bf7ff0824d0f36
ASM_URL=https://libraries.minecraft.net/org/ow2/asm/asm-all/5.0.3/asm-all-5.0.3.jar
ASM_SHA1=4333508b8dd8ee72aa4e39afa713b3a74579b773

R="$OUT/root"
rm -rf "$R"
mkdir -p "$OUT"

# Runtime packages. Xvfb is the display (input arrives through XTest from
# mc-inputd); the X libs are what LWJGL 2.9's liblwjgl.so and the JRE's AWT link.
PKGS=(
  xvfb xauth x11-xkb-utils xkb-data
  libx11-6 libxext6 libxcursor1 libxrandr2 libxxf86vm1 libxrender1 libxtst6 libxi6 libxinerama1
  libfreetype6 libfontconfig1 fonts-dejavu-core libasound2 libdrm2
  ca-certificates wget unzip procps
)
debootstrap --arch=i386 --variant=minbase --include="$(IFS=,; echo "${PKGS[*]}")" \
  "$SUITE" "$R" "$MIRROR"

# --- gl4es, the overlay and the mod: built in a throwaway copy of the root, so no toolchain ships ---
B="$OUT/build-root"
rm -rf "$B"
cp -a "$R" "$B"
git clone -q "$GL4ES_REPO" "$B/tmp/gl4es"
git -C "$B/tmp/gl4es" checkout -q "$GL4ES_REF"
for p in "$HERE"/gl4es/*.patch; do
  patch -d "$B/tmp/gl4es" -p1 --batch < "$p"
done
cp -r "$HERE/overlay" "$B/tmp/overlay"
cp -r "$HERE/mod" "$B/tmp/mod"
mount --bind /proc "$B/proc"
trap 'umount "$B/proc" 2>/dev/null || true' EXIT
cp /etc/resolv.conf "$B/etc/resolv.conf"
chroot "$B" sh -ec "
  apt-get update -q
  DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends build-essential cmake libx11-dev \\
    libfreetype-dev pkg-config openjdk-17-jdk-headless
  cd /tmp/gl4es && mkdir -p build && cd build
  # -march=bonnell: the EA's Atom CE5310 (SSSE3, MOVBE). -DNO_GBM: no GBM here.
  cmake .. -DCMAKE_BUILD_TYPE=RelWithDebInfo -DNO_GBM=ON -DUSE_CLOCK=ON \\
        -DCMAKE_C_FLAGS='-march=bonnell -mtune=bonnell'
  make -j\$(nproc)
  # The overlay is LWJGL's libGL.so.1 and needs gl4es -- also libGL.so.1 -- under
  # it. It names gl4es by path: link against a stub whose soname is that path.
  cd /tmp/overlay
  gcc -shared -o libgl4es-stub.so -Wl,-soname,/usr/lib/gl4es/libGL.so.1 -x c /dev/null
  gcc -O2 -march=bonnell -Wall -shared -fPIC -Wl,-soname,libGL.so.1 -o libGL.so.1 mc-overlay.c \\
      -I/tmp/gl4es/include \$(pkg-config --cflags freetype2) \\
      -Wl,--no-as-needed ./libgl4es-stub.so \$(pkg-config --libs freetype2) -ldl
  # openHC's launchwrapper tweaker, for the game's Java 8
  cd /tmp/mod
  wget -q $LW_URL $ASM_URL
  printf '%s  %s\\n' $LW_SHA1 launchwrapper-1.12.jar $ASM_SHA1 asm-all-5.0.3.jar | sha1sum -c --quiet
  javac --release 8 -cp launchwrapper-1.12.jar:asm-all-5.0.3.jar -d classes \$(find src -name '*.java')
  jar cf openhc-mc.jar -C classes .
"
umount "$B/proc"; trap - EXIT
install -D -m 0755 "$B/tmp/gl4es/lib/libGL.so.1" "$R/usr/lib/gl4es/libGL.so.1"
install -D -m 0755 "$B/tmp/overlay/libGL.so.1" "$R/usr/lib/mc-overlay/libGL.so.1"
install -D -m 0644 "$B/tmp/mod/openhc-mc.jar" "$R/usr/lib/openhc-mc/openhc-mc.jar"
rm -rf "$B"

# --- trim the runtime ---
# Xvfb depends on libGL for its GLX extension, which drags in Mesa's DRI
# drivers and LLVM (~150 MB). Nothing here uses them: the game's GL is gl4es on
# the SGX545, and Xvfb runs fine with GLX unable to find a driver.
installed() { chroot "$R" dpkg-query -W -f='${Package}\n' "$@" 2>/dev/null; }
drop=$(installed libgl1-mesa-dri libllvm15 libz3-4 libicu72 libxml2 libsensors5 libsensors-config \
       libdrm-amdgpu1 libdrm-nouveau2 libdrm-radeon1 libdrm-intel1 libglx-mesa0 libgbm1 || true)
[ -z "$drop" ] || chroot "$R" dpkg --purge --force-depends $drop
rm -rf "$R"/usr/share/doc/* "$R"/usr/share/man/* "$R"/usr/share/info/* "$R"/usr/share/locale/* \
       "$R"/var/cache/apt/*.bin "$R"/var/cache/apt/archives/*.deb "$R"/var/lib/apt/lists/*

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
