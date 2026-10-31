use std::env;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// What the release recipe builds with, from `.cargo/config.toml` and `[profile.release]`.
const TARGET: &str = "thumbv8m.main-none-eabihf";
const OPT_LEVEL: &str = "z";
/// The profile settings Cargo takes from the environment that change the code; `--config` and
/// a parent directory's `.cargo/config.toml` can set them too, unseen.
const PROFILE_ENV: [&str; 8] = [
    "CARGO_PROFILE_RELEASE_OPT_LEVEL",
    "CARGO_PROFILE_RELEASE_LTO",
    "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
    "CARGO_PROFILE_RELEASE_PANIC",
    "CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS",
    "CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS",
    "CARGO_PROFILE_RELEASE_STRIP",
    "CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_OPT_LEVEL",
];

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo::rustc-link-search={manifest}");
    println!("cargo::rustc-link-arg-bins=-Tlink.x");
    println!("cargo::rerun-if-changed=memory.x");
    println!("cargo::rerun-if-changed=build.rs");

    // The release recipe passes the ID it computed, for the build it makes; any other build
    // hashes how it differs from that build into its own.
    println!("cargo::rerun-if-env-changed=WRIST_RELEASE_BUILD_ID");
    let given = env::var("WRIST_RELEASE_BUILD_ID").ok();
    let variant = Variant::of_this_build(Path::new(&manifest), given.is_some());
    let (source, id) = match given {
        Some(id) => {
            assert!(
                variant.shown.is_empty(),
                "WRIST_RELEASE_BUILD_ID names the release recipe's build, and this one differs \
                 from it ({}); build without it",
                variant.suffix()
            );
            (id.clone(), id)
        }
        None => firmware_hash(Path::new(&manifest), &variant.hashed)
            .unwrap_or_else(|| ("unknown".into(), "unknown".into())),
    };
    println!("cargo::rustc-env=WRIST_SOURCE_ID={source}");
    println!("cargo::rustc-env=WRIST_BUILD_ID={id}");
    println!("cargo::rustc-env=WRIST_BUILD_VARIANT={}", variant.suffix());
}

/// How this build differs from the release recipe's, in what the inputs do not pin: `shown`
/// names each difference, and `hashed` says exactly what it is.
struct Variant {
    shown: Vec<String>,
    hashed: String,
}

impl Variant {
    fn of_this_build(manifest: &Path, recipe: bool) -> Variant {
        let mut v = Variant { shown: Vec::new(), hashed: String::new() };
        let var = |name: &str| env::var(name).unwrap_or_default();

        let mut features: Vec<_> = var("CARGO_CFG_FEATURE").split(',').filter(|f| !f.is_empty()).map(String::from).collect();
        features.sort();
        if !features.is_empty() {
            v.add(&features.join("."), &format!("features {}", features.join(",")));
        }
        if var("PROFILE") != "release" {
            v.add("dev", &format!("profile {} opt-level {}", var("PROFILE"), var("OPT_LEVEL")));
        } else if var("OPT_LEVEL") != OPT_LEVEL {
            v.add(&format!("opt-{}", var("OPT_LEVEL")), &format!("opt-level {}", var("OPT_LEVEL")));
        }
        let overrides: Vec<_> = PROFILE_ENV
            .iter()
            .inspect(|name| println!("cargo::rerun-if-env-changed={name}"))
            .filter_map(|name| env::var(name).ok().map(|value| format!("{name}={value}")))
            .collect();
        if !overrides.is_empty() {
            v.add("profile", &overrides.join(" "));
        }
        if var("TARGET") != TARGET {
            v.add("target", &format!("target {}", var("TARGET")));
        }

        // The toolchain rustup picks, against the one rust-toolchain.toml pins.
        let toolchain = manifest.join("../../rust-toolchain.toml");
        println!("cargo::rerun-if-changed={}", toolchain.display());
        let pinned = std::fs::read_to_string(toolchain).ok().and_then(|t| {
            t.lines().find_map(|l| Some(l.strip_prefix("channel")?.trim().strip_prefix('=')?.trim().trim_matches('"').to_string()))
        });
        let rustc = Command::new(var("RUSTC")).arg("-vV").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let rustc = rustc.unwrap_or_default();
        let release = rustc.lines().find_map(|l| l.strip_prefix("release: "));
        if release.is_none() || release != pinned.as_deref() {
            v.add("rustc", &format!("rustc {}", rustc.lines().take(3).collect::<Vec<_>>().join(" ")));
        }

        // A linker map changes no byte. The recipe's path remapping is its own, in
        // tools/firmware_build.py, and its image mounts every checkout at /src. Any other build
        // keeps its flags, and the paths the binary holds in panic messages and Cargo hashes
        // into symbols: the checkout's, the target directory's and Cargo's home.
        let flags = rustflags(&var("CARGO_ENCODED_RUSTFLAGS"));
        let flags: Vec<_> = flags
            .iter()
            .filter(|f| !(f.starts_with("-Clink-arg=-Map=") || recipe && f.starts_with("--remap-path-prefix=")))
            .cloned()
            .collect();
        if !flags.is_empty() {
            v.add("rustflags", &format!("rustflags {}", flags.join(" ")));
        }
        if !recipe {
            let out = var("OUT_DIR");
            let target = Path::new(&out).ancestors().nth(5).map(|p| p.display().to_string()).unwrap_or_default();
            let home = env::var("CARGO_HOME").unwrap_or_else(|_| format!("{}/.cargo", var("HOME")));
            v.add("local", &format!("paths {} {target} {home}", manifest.display()));
        }
        v
    }

    fn add(&mut self, shown: &str, hashed: &str) {
        self.shown.push(shown.into());
        self.hashed.push_str(hashed);
        self.hashed.push('\n');
    }

    /// `+` and the differences, as semver build metadata after the version; empty for the
    /// recipe's build.
    fn suffix(&self) -> String {
        if self.shown.is_empty() { String::new() } else { format!("+{}", self.shown.join(".")) }
    }
}

/// `CARGO_ENCODED_RUSTFLAGS` with each flag and its value as one word: `-Cname=value` and
/// `--remap-path-prefix=from=to`.
fn rustflags(encoded: &str) -> Vec<String> {
    let mut flags = Vec::new();
    let mut words = encoded.split('\x1f').filter(|w| !w.is_empty());
    while let Some(word) = words.next() {
        flags.push(match word {
            "-C" | "--codegen" => format!("-C{}", words.next().unwrap_or_default()),
            "--remap-path-prefix" => format!("--remap-path-prefix={}", words.next().unwrap_or_default()),
            w => w.strip_prefix("--codegen=").map_or_else(|| w.to_string(), |rest| format!("-C{rest}")),
        });
    }
    flags
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

/// The firmware hash of release/build-inputs.txt, as `tools/firmware_build.py` computes it,
/// and this build's ID: that hash with `variant` hashed in too. Each ends in `-dirty` when an
/// input has an uncommitted change, and then hashes what the tree holds.
fn firmware_hash(manifest: &Path, variant: &str) -> Option<(String, String)> {
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

    let mut listing = git(top, &args(&["ls-files", "-s", "-z", "--"]), &[])?;
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
    let dirty = !git(
        top,
        &args(&["status", "--porcelain", "--untracked-files=normal", "--"]),
        &[],
    )?
    .is_empty();
    if dirty {
        listing.extend(worktree(top, &args(&["ls-files", "-z", "--modified", "--deleted", "--others", "--exclude-standard", "--"]))?);
    }
    let hash = |bytes: &[u8]| {
        let hash = text(git(top, &["hash-object", "--stdin"], bytes)?)?;
        Some(format!("{}{}", hash.get(..12)?, if dirty { "-dirty" } else { "" }))
    };
    let source = hash(&listing)?;
    if variant.is_empty() {
        return Some((source.clone(), source));
    }
    listing.extend(b"variant\0");
    listing.extend(variant.as_bytes());
    Some((source, hash(&listing)?))
}

/// After `worktree\0`, each input that differs from the index, as `<blob> <path>\0`, with
/// `-` for the blob of a deleted one; `ls` lists them.
fn worktree(top: &Path, ls: &[&str]) -> Option<Vec<u8>> {
    let listed = git(top, ls, &[])?;
    let mut paths: Vec<_> = listed.split(|&b| b == 0).filter(|p| !p.is_empty()).collect();
    paths.sort_unstable();
    paths.dedup();
    let present: Vec<_> = paths
        .iter()
        .filter(|p| top.join(String::from_utf8_lossy(p).as_ref()).is_file())
        .copied()
        .collect();
    let blobs = git(top, &["hash-object", "--stdin-paths"], &present.join(&b'\n'))?;
    let mut blobs = blobs.split(|&b| b == b'\n');
    let mut out = b"worktree\0".to_vec();
    for path in paths {
        out.extend(if present.contains(&path) { blobs.next()? } else { b"-" });
        out.push(b' ');
        out.extend(path);
        out.push(0);
    }
    Some(out)
}
