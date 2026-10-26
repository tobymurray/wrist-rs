//! CLDR plural rules (UTS #35, "Language Plural Rules") to a Rust condition on a `u32`, whose
//! `v`, `w`, `f`, `t`, `c` and `e` are 0.

use std::fmt::Write as _;

/// A condition: decided now, or Rust code over `n`; `Or` is a disjunction without parentheses.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Cond {
    Const(bool),
    Code(String),
    Or(String),
}

fn and(a: Cond, b: Cond) -> Cond {
    let operand = |c| match c {
        Cond::Or(c) => format!("({c})"),
        Cond::Code(c) => c,
        Cond::Const(_) => unreachable!(),
    };
    match (a, b) {
        (Cond::Const(false), _) | (_, Cond::Const(false)) => Cond::Const(false),
        (Cond::Const(true), x) | (x, Cond::Const(true)) => x,
        (a, b) => Cond::Code(format!("{} && {}", operand(a), operand(b))),
    }
}

fn or(a: Cond, b: Cond) -> Cond {
    match (a, b) {
        (Cond::Const(true), _) | (_, Cond::Const(true)) => Cond::Const(true),
        (Cond::Const(false), x) | (x, Cond::Const(false)) => x,
        (Cond::Code(a) | Cond::Or(a), Cond::Code(b) | Cond::Or(b)) => Cond::Or(format!("{a} || {b}")),
    }
}

/// A literal Clippy reads without complaint: `1_000_000`.
fn lit(v: u32) -> String {
    let digits = v.to_string();
    if digits.len() < 5 {
        return digits;
    }
    let mut out = String::new();
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('_');
        }
        out.push(d);
    }
    out
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
    // `!=` holds when no item matches: each item negated, joined with `and`.
    let mut all = Cond::Const(negated);
    for item in rhs.split(',').map(str::trim) {
        let (lo, hi) = match item.split_once("..") {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (item, item),
        };
        let lo: u32 = lo.parse().map_err(|_| format!("bad value `{lo}` in `{text}`"))?;
        let hi: u32 = hi.parse().map_err(|_| format!("bad value `{hi}` in `{text}`"))?;
        let (not, eq) = if negated { ("!", "!=") } else { ("", "==") };
        let hit = match &operand {
            Operand::Zero => Cond::Const((lo == 0) != negated),
            Operand::N { modulo: Some(m) } if lo == 0 && hi == 0 => Cond::Code(format!("{not}n.is_multiple_of({})", lit(*m))),
            Operand::N { modulo } => {
                let x = modulo.map_or_else(|| "n".to_string(), |m| format!("n % {}", lit(m)));
                if lo == hi {
                    Cond::Code(format!("{x} {eq} {}", lit(lo)))
                } else {
                    let x = if modulo.is_some() { format!("&({x})") } else { format!("&{x}") };
                    Cond::Code(format!("{not}({}..={}).contains({x})", lit(lo), lit(hi)))
                }
            }
        };
        all = if negated { and(all, hit) } else { or(all, hit) };
    }
    Ok(all)
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
            Cond::Code(c) | Cond::Or(c) => write!(body, "if {c} {{\n        return {variant};\n    }}\n    ").unwrap_or(()),
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
        assert!(b.contains("if n % 10 == 1 && n % 100 != 11 {"), "{b}");
        assert!(b.contains("if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) {"), "{b}");
        assert!(b.contains("if n.is_multiple_of(10) || (5..=9).contains(&(n % 10)) || (11..=14).contains(&(n % 100)) {"), "{b}");
    }

    #[test]
    fn french_lists_and_large_moduli_read_as_clippy_asks() {
        let b = body(&[
            ("one", "i = 0,1"),
            ("many", "e = 0 and i != 0 and i % 1000000 = 0 and v = 0 or e != 0..5"),
        ]);
        assert!(b.contains("if n == 0 || n == 1 {"), "{b}");
        assert!(b.contains("if n != 0 && n.is_multiple_of(1_000_000) {"), "{b}");
    }

    #[test]
    fn a_list_inside_and_keeps_its_parentheses() {
        assert_eq!(condition("i = 0,1 and n % 10 = 2").unwrap(), Cond::Code("(n == 0 || n == 1) && n % 10 == 2".into()));
    }

    #[test]
    fn nonsense_is_refused() {
        assert!(condition("x = 1").is_err());
        assert!(condition("n > 1").is_err());
        assert!(variant("several").is_err());
    }
}
