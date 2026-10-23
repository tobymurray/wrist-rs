use std::env;
use std::process::Command;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo::rustc-link-search={manifest}");
    println!("cargo::rustc-link-arg-bins=-Tlink.x");
    println!("cargo::rerun-if-changed=memory.x");
    println!("cargo::rerun-if-changed=build.rs");

    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&manifest)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    println!("cargo::rerun-if-env-changed=WRIST_RELEASE_BUILD_ID");
    let commit = env::var("WRIST_RELEASE_BUILD_ID")
        .ok()
        .or_else(|| git(&["rev-parse", "--short=12", "HEAD"]))
        .unwrap_or_else(|| "unknown".into());
    let dirty =
        git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    let id = if dirty {
        format!("{commit}-dirty")
    } else {
        commit
    };
    println!("cargo::rustc-env=WRIST_BUILD_ID={id}");
    println!("cargo::rerun-if-changed=../../.git/HEAD");
    println!("cargo::rerun-if-changed=../../.git/index");
}
