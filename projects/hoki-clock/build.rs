fn main() {
    slint_build::compile("ui/main.slint").unwrap();
    cc::Build::new().file("../hoki-haptics/src/evdev.c").compile("clock_haptics");
    println!("cargo:rerun-if-changed=../hoki-haptics/src/evdev.c");
}
