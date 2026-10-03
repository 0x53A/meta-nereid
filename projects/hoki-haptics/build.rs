fn main() {
    println!("cargo:rerun-if-changed=src/evdev.c");
    cc::Build::new()
        .file("src/evdev.c")
        .warnings(true)
        .compile("haptics_evdev");
    slint_build::compile("ui/main.slint").unwrap();
}
