SUMMARY = "Source-built WebAssembly guest and its matching Rust standard library"
LICENSE = "CLOSED & (MIT | Apache-2.0) & Unicode-3.0"
LIC_FILES_CHKSUM = "file://../../COPYRIGHT;md5=11a3899825f4376896e438c8c753f8dc"
# Match the compiler supplied by this OE release; no rustup or host cache inputs.
SRC_URI = "https://static.rust-lang.org/dist/rustc-${PV}-src.tar.xz;name=rust \
           file://hoki-wasm-guest/src;subdir=projects"
SRC_URI[rust.sha256sum] = "6bfeaddd90ffda2f063492b092bfed925c4b8c701579baf4b1316e021470daac"
FILESEXTRAPATHS:prepend := "${THISDIR}/../../projects:"
S = "${UNPACKDIR}/rustc-${PV}-src/library/sysroot"
inherit cargo native
DEPENDS += "lld-native"
CARGO_DISABLE_BITBAKE_VENDORING = "0"
CARGO_VENDORING_DIRECTORY = "${UNPACKDIR}/rustc-${PV}-src/vendor"

do_compile() {
    rustc --version | grep -q "^rustc ${PV} " || bbfatal "Update WASM std source to match rust-native"
    export RUSTC_BOOTSTRAP=1
    export RUSTFLAGS="-Cembed-bitcode=yes -Zforce-unstable-if-unmarked"
    cargo build --frozen --release --manifest-path=${S}/Cargo.toml --target wasm32-unknown-unknown --no-default-features --features compiler-builtins-mem
    install -d ${B}/wasm-sysroot/lib/rustlib/wasm32-unknown-unknown/lib
    cp ${B}/target/wasm32-unknown-unknown/release/deps/*.rlib ${B}/wasm-sysroot/lib/rustlib/wasm32-unknown-unknown/lib/
    rustc --edition=2021 --crate-type=cdylib --crate-name hoki_wasm_guest \
        --target wasm32-unknown-unknown --sysroot ${B}/wasm-sysroot \
        -C linker=wasm-ld -C opt-level=z -C panic=abort \
        ${UNPACKDIR}/projects/hoki-wasm-guest/src/lib.rs -o ${B}/guest.wasm
}
do_install() {
    install -Dm0644 ${B}/guest.wasm ${D}${datadir}/hoki-wasm/guest.wasm
}
