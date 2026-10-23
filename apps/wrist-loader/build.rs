use std::env;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo::rustc-link-search={manifest}");
    println!("cargo::rustc-link-arg-bins=-Tlink.x");
    println!("cargo::rerun-if-changed=memory.x");
    println!("cargo::rerun-if-changed=build.rs");
}
