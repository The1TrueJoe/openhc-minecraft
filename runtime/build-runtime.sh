#!/usr/bin/env bash
# Build the game runtime: a Debian trixie amd64 root with Xorg, the open Mesa
# for the SGX545 (powervr_sgx's mesa-sgx package), the X client libraries
# LWJGL 2 needs, a 64-bit Java 8 and the launcher scripts.
# Output: runtime-amd64.tar.xz.
#
# The game draws with desktop GL 2.1 straight from Mesa's sgx driver: Xorg runs
# on the panel (modesetting on the boot framebuffer, glamor on the GPU) and the
# game is an ordinary GLX client of it. Needs a box on the powervr_sgx kernel
# driver at the same release as MESA_VERSION.
#
# Contains no Mojang files: the game is fetched on the box by mc-install from
# Mojang's servers, using the file list made here (mcfiles.py).
#
# Needs root (debootstrap, chroot) on an x86_64 Linux host.
#
#   build-runtime.sh OUT_DIR
set -euo pipefail

OUT="$(realpath "${1:?usage: build-runtime.sh OUT_DIR}")"
HERE="$(cd "$(dirname "$0")" && pwd)"
SUITE=trixie
MIRROR=http://deb.debian.org/debian
MC_VERSION=1.7.10
JRE_URL=https://cdn.azul.com/zulu/bin/zulu8.98.0.107-ca-jre8.0.512-linux_x64.tar.gz
# Mesa with the sgx driver, from the powervr_sgx release the box's kernel driver is at.
MESA_VERSION=0.2.1
MESA_DEB=mesa-sgx_26.2.5+sgx${MESA_VERSION}_amd64.deb
MESA_URL=https://github.com/The1TrueJoe/powervr_sgx/releases/download/v${MESA_VERSION}/${MESA_DEB}
MESA_SHA256=4c445f97cbca686520d0e1192eb7c53525b18bf2cd331b5f07738210bc6999af
# What runtime/mod compiles against (the box fetches the same jars with the game).
LW_URL=https://libraries.minecraft.net/net/minecraft/launchwrapper/1.12/launchwrapper-1.12.jar
LW_SHA1=111e7bea9c968cdb3d06ef4632bf7ff0824d0f36
ASM_URL=https://libraries.minecraft.net/org/ow2/asm/asm-all/5.0.3/asm-all-5.0.3.jar
ASM_SHA1=4333508b8dd8ee72aa4e39afa713b3a74579b773
# mc-record's encoder: a small static ffmpeg with only what recording uses.
FFMPEG_VERSION=7.1.5
FFMPEG_URL=https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz
FFMPEG_SHA256=de668509caf9e35e3cd162473441fdb29538c6d96ed080292b3cf9e6fc5d558f

R="$OUT/root"
rm -rf "$R"
mkdir -p "$OUT"

# Runtime packages. Xorg is the display (input arrives through XTest from
# mc-inputd); the X libs are what LWJGL 2.9's liblwjgl64.so and the JRE's AWT link.
PKGS=(
  xserver-xorg-core x11-xkb-utils xkb-data
  libgl1 libglx0 libegl1 libgles2
  libx11-6 libxext6 libxcursor1 libxrandr2 libxxf86vm1 libxrender1 libxtst6 libxi6 libxinerama1
  libfreetype6 libfontconfig1 fonts-dejavu-core libasound2t64 libdrm2 libx264-164
  ca-certificates wget unzip procps
)
debootstrap --arch=amd64 --variant=minbase --include="$(IFS=,; echo "${PKGS[*]}")" \
  "$SUITE" "$R" "$MIRROR"

mount --bind /proc "$R/proc"
trap 'umount "$R/proc" 2>/dev/null || true' EXIT
cp /etc/resolv.conf "$R/etc/resolv.conf"

# --- Mesa for the SGX545 (installs in /opt/sgx, ahead of Debian's through ld.so.conf and glvnd) ---
wget -q -O "$R/tmp/$MESA_DEB" "$MESA_URL"
echo "$MESA_SHA256  $R/tmp/$MESA_DEB" | sha256sum -c --quiet
chroot "$R" sh -ec "
  apt-get update -q
  DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends /tmp/$MESA_DEB
"
rm -f "$R/tmp/$MESA_DEB"
umount "$R/proc"; trap - EXIT

# --- the overlay, the mod and ffmpeg: built in a throwaway copy of the root, so no toolchain ships ---
B="$OUT/build-root"
rm -rf "$B"
cp -a "$R" "$B"
cp -r "$HERE/overlay" "$B/tmp/overlay"
cp -r "$HERE/mod" "$B/tmp/mod"
mount --bind /proc "$B/proc"
trap 'umount "$B/proc" 2>/dev/null || true' EXIT
chroot "$B" sh -ec "
  DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends build-essential libgl-dev \\
    libfreetype-dev pkg-config openjdk-21-jdk-headless nasm libx264-dev libasound2-dev xz-utils
  # The overlay is LWJGL's libGL.so.1 and needs the real one -- also libGL.so.1 --
  # under it. It names that by path: link against a stub whose soname is the path.
  # -march=bonnell: the EA's Atom CE5310 (SSSE3, MOVBE).
  cd /tmp/overlay
  gcc -shared -o libgl-stub.so -Wl,-soname,/usr/lib/x86_64-linux-gnu/libGL.so.1 -x c /dev/null
  gcc -O2 -march=bonnell -Wall -shared -fPIC -Wl,-soname,libGL.so.1 -o libGL.so.1 mc-overlay.c capture.c \\
      \$(pkg-config --cflags freetype2) \\
      -Wl,--no-as-needed ./libgl-stub.so \$(pkg-config --libs freetype2) -ldl -lpthread
  # openHC's launchwrapper tweaker, for the game's Java 8
  cd /tmp/mod
  wget -q $LW_URL $ASM_URL
  printf '%s  %s\\n' $LW_SHA1 launchwrapper-1.12.jar $ASM_SHA1 asm-all-5.0.3.jar | sha1sum -c --quiet
  javac --release 8 -cp launchwrapper-1.12.jar:asm-all-5.0.3.jar -d classes \$(find src -name '*.java')
  jar cf openhc-mc.jar -C classes .
  # ffmpeg for mc-record: raw I420 from a FIFO and the ALSA loopback in,
  # x264 + AAC out to MP4/Matroska
  cd /tmp
  wget -q $FFMPEG_URL
  echo '$FFMPEG_SHA256  ffmpeg-$FFMPEG_VERSION.tar.xz' | sha256sum -c --quiet
  tar xJf ffmpeg-$FFMPEG_VERSION.tar.xz
  cd ffmpeg-$FFMPEG_VERSION
  ./configure --disable-everything --disable-autodetect --disable-doc --disable-debug \\
      --disable-ffplay --disable-ffprobe --disable-network --enable-gpl --enable-libx264 --enable-alsa \\
      --enable-encoder=libx264,aac --enable-decoder=rawvideo,pcm_s16le --enable-demuxer=rawvideo \\
      --enable-muxer=mp4,matroska --enable-protocol=file,pipe --enable-indev=alsa \\
      --enable-filter=buffer,buffersink,abuffer,abuffersink,aresample,aformat,format,null,anull,scale,fps \\
      --extra-cflags='-march=bonnell -mtune=bonnell' >/dev/null
  make -j\$(nproc) ffmpeg >/dev/null
  strip ffmpeg
"
umount "$B/proc"; trap - EXIT
install -D -m 0755 "$B/tmp/overlay/libGL.so.1" "$R/usr/lib/mc-overlay/libGL.so.1"
install -D -m 0644 "$B/tmp/mod/openhc-mc.jar" "$R/usr/lib/openhc-mc/openhc-mc.jar"
install -D -m 0755 "$B/tmp/ffmpeg-$FFMPEG_VERSION/ffmpeg" "$R/usr/local/bin/ffmpeg"
rm -rf "$B"

# --- trim the runtime ---
# Debian's own Mesa came in under Xorg and glvnd; mesa-sgx in /opt/sgx replaces
# all of it (its libgbm, libGLX_mesa and libEGL_mesa come first on the library
# path), so drop Debian's drivers and their dependencies.
installed() { chroot "$R" dpkg-query -W -f='${Package}\n' "$@" 2>/dev/null; }
drop=$(installed mesa-libgallium libgl1-mesa-dri libglx-mesa0 libegl-mesa0 libgbm1 mesa-vulkan-drivers \
       libvulkan1 libdrm-amdgpu1 libdrm-nouveau2 libdrm-radeon1 libdrm-intel1 || true)
[ -z "$drop" ] || chroot "$R" dpkg --purge --force-depends $drop
chroot "$R" ldconfig
rm -rf "$R"/usr/share/doc/* "$R"/usr/share/man/* "$R"/usr/share/info/* "$R"/usr/share/locale/* \
       "$R"/var/cache/apt/*.bin "$R"/var/cache/apt/archives/*.deb "$R"/var/lib/apt/lists/*

# --- Java 8, 64-bit ---
mkdir -p "$R/opt/java"
wget -q -O- "$JRE_URL" | tar xz -C "$R/opt/java"
ln -sfn "$(basename "$JRE_URL" .tar.gz)" "$R/opt/java/jre8"

# --- the game's file list (URLs only), launcher scripts, Xorg's config ---
mkdir -p "$R/opt/minecraft"
( cd "$R/opt/minecraft" && python3 "$HERE/mcfiles.py" "$MC_VERSION" 64 )
install -m 0755 "$HERE"/bin/* "$R/usr/local/bin/"
install -D -m 0644 "$HERE/xorg.conf" "$R/etc/X11/xorg.conf"

# Leave no host state behind.
rm -f "$R/etc/resolv.conf"
echo "openhc-minecraft runtime, $SUITE amd64, mesa-sgx $MESA_VERSION, minecraft $MC_VERSION, built $(date -u +%FT%TZ)" \
  > "$R/etc/openhc-minecraft-runtime"

tar -C "$R" -cf - . | xz -T0 -6 > "$OUT/runtime-amd64.tar.xz"
ls -l "$OUT/runtime-amd64.tar.xz"
