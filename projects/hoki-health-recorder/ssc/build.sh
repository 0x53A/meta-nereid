#!/bin/sh
set -eu
cd "$(dirname "$0")"
ndk_root=${ANDROID_NDK_ROOT:-/home/lukas/Android/Sdk/ndk/29.0.14206865}
compiler="$ndk_root/toolchains/llvm/prebuilt/linux-x86_64/bin/armv7a-linux-androideabi28-clang"
test -x "$compiler"
mkdir -p build
"$compiler" -std=c11 -Wall -Wextra -Werror -pthread -O2 collector.c -ldl -o build/hoki-ssc-recorder
sha256sum build/hoki-ssc-recorder > build/SHA256SUMS
"$compiler" --version > build/compiler.txt
