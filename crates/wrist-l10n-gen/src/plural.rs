//! CLDR plural rules (UTS #35, "Language Plural Rules") to a Rust condition on a `u32`.
//!
//! The watch only counts whole numbers, so the operands are an integer's: `n` and `i` are the
//! number, and `v`, `w`, `f`, `t`, `c` and `e` are 0. Relations on those are decided here, so
//! the generated code has no constant comparisons in it.

use std::fmt::Write as _;

/// A condition: decided now, or Rust code over `n`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Cond {
    Const(bool),
    Code(String),
}

fn and(a: Cond, b: Cond) -> Cond {
    match (a, b) {
        (Cond::Const(false), _) | (_, Cond::Const(false)) => Cond::Const(false),
        (Cond::Const(true), x) | (x, Cond::Const(true)) => x,
        (Cond::Code(a), Cond::Code(b)) => Cond::Code(format!("{a} && {b}")),
    }
}

fn or(a: Cond, b: Cond) -> Cond {
    match (a, b) {
        (Cond::Const(true), _) | (_, Cond::Const(true)) => Cond::Const(true),
        (Cond::Const(false), x) | (x, Cond::Const(false)) => x,
        (Cond::Code(a), Cond::Code(b)) => Cond::Code(format!("({a} || {b})")),
    }
}

/// `n` (as code) or a constant operand.
enum Operand {
    N { modulo: Option<u32> },
    Zero,
}

/// One relation: `operand [% m] (=|!=) list`.
fn relation(text: &str) -> Result<Cond, String> {
    let (lhs, rhs, negated) = if let Some((l, r)) = text.split_once("!=") {
        (l, r, true)
    } else if let Some((l, r)) = text.split_once('=') {
        (l, r, false)
    } else {
        return Err(format!("no = or != in `{text}`"));
    };
    let mut lhs = lhs.split('%').map(str::trim);
    let operand = match lhs.next() {
        Some("n" | "i") => Operand::N { modulo: None },
        Some("v" | "w" | "f" | "t" | "c" | "e") => Operand::Zero,
        other => return Err(format!("unknown operand {other:?} in `{text}`")),
    };
    let operand = match (operand, lhs.next()) {
        (Operand::N { .. }, Some(m)) => Operand::N { modulo: Some(m.parse().map_err(|_| format!("bad modulus in `{text}`"))?) },
        (o, _) => o,
    };
    let mut any = Cond::Const(false);
    for item in rhs.split(',').map(str::trim) {
        let (lo, hi) = match item.split_once("..") {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (item, item),
        };
        let lo: u32 = lo.parse().map_err(|_| format!("bad value `{lo}` in `{text}`"))?;
        let hi: u32 = hi.parse().map_err(|_| format!("bad value `{hi}` in `{text}`"))?;
        let hit = match &operand {
            Operand::Zero => Cond::Const(lo == 0),
            Operand::N { modulo } => {
                let x = modulo.map_or_else(|| "n".to_string(), |m| format!("n % {m}"));
                if lo == hi {
                    Cond::Code(format!("{x} == {lo}"))
                } else {
                    Cond::Code(format!("({lo}..={hi}).contains(&({x}))"))
                }
            }
        };
        any = or(any, hit);
    }
    Ok(match (any, negated) {
        (Cond::Const(b), true) => Cond::Const(!b),
        (Cond::Code(c), true) => Cond::Code(format!("!({c})")),
        (c, false) => c,
    })
}

/// A rule's condition, without its `@integer` and `@decimal` samples.
fn condition(rule: &str) -> Result<Cond, String> {
    let rule = rule.split('@').next().unwrap_or("").trim();
    let mut any = Cond::Const(false);
    for alternative in rule.split(" or ") {
        let mut all = Cond::Const(true);
        for rel in alternative.split(" and ") {
            all = and(all, relation(rel.trim())?);
        }
        any = or(any, all);
    }
    Ok(any)
}

/// The body of `fn(n: u32) -> Plural` for `rules`, as `(category, condition)` in CLDR's
/// order; `other` is what is left.
///
/// # Errors
/// A rule outside the grammar above.
pub fn function_body(rules: &[(String, String)]) -> Result<String, String> {
    let mut body = String::new();
    for (category, rule) in rules {
        let variant = variant(category)?;
        match condition(rule)? {
            Cond::Const(false) => {}
            Cond::Const(true) => return Ok(format!("{body}{variant}")),
            Cond::Code(c) => write!(body, "if {c} {{\n        return {variant};\n    }}\n    ").unwrap_or(()),
        }
    }
    Ok(format!("{body}Plural::Other"))
}

/// `Plural::One` for `one`.
///
/// # Errors
/// A name CLDR does not have.
pub fn variant(category: &str) -> Result<&'static str, String> {
    Ok(match category {
        "zero" => "Plural::Zero",
        "one" => "Plural::One",
        "two" => "Plural::Two",
        "few" => "Plural::Few",
        "many" => "Plural::Many",
        "other" => "Plural::Other",
        _ => return Err(format!("no plural category `{category}`")),
    })
}

/// CLDR's order of categories.
pub const ORDER: [&str; 6] = ["zero", "one", "two", "few", "many", "other"];

#[cfg(test)]
mod tests {
    use super::*;

    fn body(rules: &[(&str, &str)]) -> String {
        let rules: Vec<_> = rules.iter().map(|(c, r)| ((*c).to_string(), (*r).to_string())).collect();
        function_body(&rules).unwrap()
    }

    #[test]
    fn english_folds_its_visible_digits_away() {
        assert_eq!(body(&[("one", "i = 1 and v = 0 @integer 1")]), "if n == 1 {\n        return Plural::One;\n    }\n    Plural::Other");
    }

    #[test]
    fn a_rule_that_needs_fractions_never_matches_an_integer() {
        assert_eq!(body(&[("one", "v != 0")]), "Plural::Other");
    }

    #[test]
    fn russian_ranges_moduli_and_negation() {
        let b = body(&[
            ("one", "v = 0 and i % 10 = 1 and i % 100 != 11"),
            ("few", "v = 0 and i % 10 = 2..4 and i % 100 != 12..14"),
            ("many", "v = 0 and i % 10 = 0 or v = 0 and i % 10 = 5..9 or v = 0 and i % 100 = 11..14"),
        ]);
        assert!(b.contains("n % 10 == 1 && !(n % 100 == 11)"), "{b}");
        assert!(b.contains("(2..=4).contains(&(n % 10)) && !((12..=14).contains(&(n % 100)))"), "{b}");
        assert!(b.contains("((n % 10 == 0 || (5..=9).contains(&(n % 10))) || (11..=14).contains(&(n % 100)))"), "{b}");
    }

    #[test]
    fn lists_join_with_or() {
        assert_eq!(condition("n = 0,1").unwrap(), Cond::Code("(n == 0 || n == 1)".into()));
    }

    #[test]
    fn nonsense_is_refused() {
        assert!(condition("x = 1").is_err());
        assert!(condition("n > 1").is_err());
        assert!(variant("several").is_err());
    }
}
