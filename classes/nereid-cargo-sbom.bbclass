# Cargo's per-artifact SBOM precursor records the crates actually compiled for
# each output, including the target, enabled features, and build dependencies.
# This opt-in is local to BitBake; ordinary project builds need no Cargo flags.
inherit deploy

# OE's pinned Cargo is stable. Cargo's SBOM feature is still gated by -Z, but
# needs no unstable Rust source features. Keep the gate scoped to these recipes.
export RUSTC_BOOTSTRAP = "1"
export CARGO_BUILD_SBOM = "true"
CARGO_BUILD_FLAGS:append = " -Z sbom"
do_deploy[file-checksums] += "${NEREID_LAYER_ROOT}/tools/cargo-sbom-licenses.py:True"

do_deploy() {
    sbom_src="${B}/target/${CARGO_TARGET_SUBDIR}"
    sbom_dst="${DEPLOYDIR}/cargo-sbom/${PN}"
    install -d "$sbom_dst"
    found=0
    for report in "$sbom_src"/*.cargo-sbom.json; do
        [ -f "$report" ] || continue
        install -m 0644 "$report" "$sbom_dst/"
        found=1
    done
    [ "$found" = 1 ] || bbfatal "Cargo produced no SBOM precursors in $sbom_src"
    python3 "${NEREID_LAYER_ROOT}/tools/cargo-sbom-licenses.py" \
        --manifest "${S}/Cargo.toml" --precursors "$sbom_src" --output "$sbom_dst"
}
addtask deploy after do_compile before do_package
