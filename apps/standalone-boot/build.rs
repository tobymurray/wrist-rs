use std::env;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo::rustc-link-search={manifest}");
    println!("cargo::rustc-link-arg-bins=-Tlink.x");
    println!("cargo::rerun-if-changed=memory.x");
    println!("cargo::rerun-if-changed=build.rs");

    // The release recipe passes the ID it computed; a build outside it computes the same.
    println!("cargo::rerun-if-env-changed=WRIST_RELEASE_BUILD_ID");
    let id = env::var("WRIST_RELEASE_BUILD_ID")
        .ok()
        .or_else(|| firmware_hash(Path::new(&manifest)))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo::rustc-env=WRIST_BUILD_ID={id}");
}

fn git(dir: &Path, args: &[&str], stdin: &[u8]) -> Option<Vec<u8>> {
    // No index refresh, which would rewrite the index this script watches.
    let mut child = Command::new("git")
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(stdin).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status.success().then_some(out.stdout)
}

/// The build ID of release/build-inputs.txt, as `tools/firmware_build.py` computes it: the
/// hash of the inputs' index entries, and `-dirty` when any input has an uncommitted change.
fn firmware_hash(manifest: &Path) -> Option<String> {
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).ok().map(|s| s.trim().to_string());
    let top = text(git(manifest, &["rev-parse", "--show-toplevel"], &[])?)?;
    let top = Path::new(&top);
    let list = top.join("release/build-inputs.txt");
    println!("cargo::rerun-if-changed={}", list.display());
    let list = std::fs::read_to_string(list).ok()?;
    let specs = list
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'));
    let args = |head: &[&'static str]| {
        head.iter()
            .copied()
            .chain(specs.clone())
            .collect::<Vec<_>>()
    };

    let listing = git(top, &args(&["ls-files", "-s", "-z", "--"]), &[])?;
    // Commits and staging rewrite the index; an edit to an input changes its file.
    let index = text(git(
        top,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        &[],
    )?)?;
    println!("cargo::rerun-if-changed={index}");
    for entry in listing.split(|&b| b == 0).filter(|e| !e.is_empty()) {
        let path = entry.splitn(2, |&b| b == b'\t').nth(1)?;
        println!(
            "cargo::rerun-if-changed={}",
            top.join(String::from_utf8_lossy(path).as_ref()).display()
        );
    }
    let hash = text(git(top, &["hash-object", "--stdin"], &listing)?)?;
    let dirty = !git(
        top,
        &args(&["status", "--porcelain", "--untracked-files=normal", "--"]),
        &[],
    )?
    .is_empty();
    Some(format!(
        "{}{}",
        hash.get(..12)?,
        if dirty { "-dirty" } else { "" }
    ))
}
