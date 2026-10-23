//! No user-visible text outside `src/strings.rs`: every string or byte-string literal in the
//! crate's other sources is either an assertion's message, inside a `#[cfg(test)]` module, or
//! on a line that says `// verbatim` (data such as hex digits, never words).

use std::fs;
use std::path::Path;

const ASSERTS: [&str; 5] = ["assert!", "assert_eq!", "assert_ne!", "debug_assert!", "panic!"];

/// `(line, literal)` for each literal outside comments, assertions and test modules.
fn literals(source: &str) -> Vec<(usize, String)> {
    let src = source.split("#[cfg(test)]").next().unwrap_or("");
    let b = src.as_bytes();
    let (mut i, mut line) = (0, 1);
    let mut found = Vec::new();
    // Parenthesis depth at which each open assertion macro started.
    let (mut depth, mut asserts): (i32, Vec<i32>) = (0, Vec::new());
    while i < b.len() {
        match b[i] {
            b'\n' => line += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    line += usize::from(b[i] == b'\n');
                    i += 1;
                }
                i += 2;
                continue;
            }
            b'\'' => {
                // A char literal ('x', '\n', '"'); otherwise a lifetime.
                if b.get(i + 1) == Some(&b'\\') {
                    i += 2;
                    while i < b.len() && b[i] != b'\'' {
                        i += 1;
                    }
                } else if b.get(i + 2) == Some(&b'\'') {
                    i += 2;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if asserts.last() == Some(&depth) {
                    asserts.pop();
                }
            }
            b'"' => {
                let (start, at) = (i, line);
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    line += usize::from(b[i] == b'\n');
                    i += 1;
                }
                if asserts.is_empty() {
                    found.push((at, src[start..=i.min(b.len() - 1)].to_string()));
                }
            }
            _ => {
                if let Some(m) = ASSERTS.iter().find(|m| src[i..].starts_with(**m)) {
                    let before = if i == 0 { b' ' } else { b[i - 1] };
                    if !(before.is_ascii_alphanumeric() || before == b'_') {
                        asserts.push(depth);
                        i += m.len();
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    let lines: Vec<&str> = src.lines().collect();
    found.retain(|(n, _)| !lines.get(n - 1).is_some_and(|l| l.contains("// verbatim")));
    found
}

#[test]
fn no_text_outside_the_strings_module() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stray = Vec::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") || path.ends_with("strings.rs") {
            continue;
        }
        for (n, lit) in literals(&fs::read_to_string(&path).unwrap()) {
            stray.push(format!("{}:{n}: {lit}", path.file_name().unwrap().to_string_lossy()));
        }
    }
    assert!(stray.is_empty(), "literals outside strings.rs (move them, or mark data `// verbatim`):\n{}", stray.join("\n"));
}

#[test]
fn the_scanner_finds_what_it_should() {
    let src = r#"
fn a() { l.push("word"); }
// "a comment"
/* "a block
   comment" */
fn b() { let c = '"'; let d = b"bytes"; }
fn e<'a>(x: &'a str) { assert!(x.len() < 3, "message {}", f("inner")); }
fn g() { h("data"); } // verbatim
fn i() { j("after\"quote"); }
#[cfg(test)]
mod tests { fn k() { l("test"); } }
"#;
    let found = literals(src);
    let text: Vec<&str> = found.iter().map(|(_, l)| l.as_str()).collect();
    assert_eq!(text, ["\"word\"", "\"bytes\"", "\"after\\\"quote\""]);
    assert_eq!(found.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [2, 6, 9]);
}
