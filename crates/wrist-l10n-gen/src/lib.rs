//! Compiles `l10n/` into `crates/wrist-l10n/src/generated.rs`: the messages from Fluent, and
//! each locale's names, patterns and plural rule from CLDR through ICU4X.
//!
//! The Fluent subset (`l10n/README.md`): plain text on one line, `{ $variable }`, terms without
//! arguments, string literals, and a selector on a number (`{ $n -> [one] ... *[other] ... }`)
//! whose default is `other`. Anything else is refused with the message's id.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use fluent_syntax::ast;
use icu::calendar::{Date, Gregorian};
use icu::datetime::pattern::{DateTimePattern, FixedCalendarDateTimeNames, MonthNameLength, WeekdayNameLength};
use icu::datetime::{fieldsets, FixedCalendarDateTimeFormatter, NoCalendarFormatter};
use serde::Deserialize;
use writeable::TryWriteable;

pub mod plural;

/// The file the generator writes, relative to the repository's root.
pub const OUTPUT: &str = "crates/wrist-l10n/src/generated.rs";

/// The source catalogue: every message, and the order of each message's arguments.
const SOURCE: &str = "en.ftl";

#[derive(Deserialize)]
struct Ids {
    messages: BTreeMap<String, u16>,
}

#[derive(Deserialize)]
struct Locales {
    locale: Vec<LocaleEntry>,
}

#[derive(Deserialize)]
pub struct LocaleEntry {
    pub id: u8,
    pub tag: String,
    catalogue: String,
    plural: BTreeMap<String, String>,
    /// Month names that replace CLDR's, by month number, each with its reason in a comment
    /// beside it in `locales.toml`.
    #[serde(default)]
    pub month_overrides: BTreeMap<String, String>,
}

/// The built-in locales, as `locales.toml` lists them.
///
/// # Errors
/// An unreadable or malformed `locales.toml`.
pub fn locales(root: &Path) -> Result<Vec<LocaleEntry>, String> {
    let text = read(&root.join("l10n/locales.toml"))?;
    let l: Locales = toml::from_str(&text).map_err(|e| format!("locales.toml: {e}"))?;
    Ok(l.locale)
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// A compiled piece, before it is written out as Rust.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    Text(String),
    Arg(u8),
    Select { arg: u8, cases: Vec<(String, Vec<Piece>)>, other: Vec<Piece> },
}

struct Catalogue {
    messages: BTreeMap<String, (Vec<Piece>, Option<String>)>,
}

/// Arguments in order of first appearance in `pattern`.
fn collect_vars(pattern: &ast::Pattern<&str>, vars: &mut Vec<String>) {
    for element in &pattern.elements {
        if let ast::PatternElement::Placeable { expression } = element {
            match expression {
                ast::Expression::Inline(inline) => collect_inline(inline, vars),
                ast::Expression::Select { selector, variants } => {
                    collect_inline(selector, vars);
                    for v in variants {
                        collect_vars(&v.value, vars);
                    }
                }
            }
        }
    }
}

fn collect_inline(inline: &ast::InlineExpression<&str>, vars: &mut Vec<String>) {
    match inline {
        ast::InlineExpression::VariableReference { id } => {
            if !vars.iter().any(|v| v == id.name) {
                vars.push(id.name.to_string());
            }
        }
        ast::InlineExpression::FunctionReference { arguments, .. } => {
            for a in &arguments.positional {
                collect_inline(a, vars);
            }
        }
        _ => {}
    }
}

struct Compiler<'a> {
    id: &'a str,
    vars: &'a [String],
    terms: &'a BTreeMap<String, Vec<Piece>>,
}

impl Compiler<'_> {
    fn err(&self, what: &str) -> String {
        format!("message `{}`: {what}", self.id)
    }

    fn pattern(&self, pattern: &ast::Pattern<&str>) -> Result<Vec<Piece>, String> {
        let mut out = Vec::new();
        for element in &pattern.elements {
            match element {
                ast::PatternElement::TextElement { value } => {
                    if value.contains('\n') {
                        return Err(self.err("a message is one line"));
                    }
                    out.push(Piece::Text((*value).to_string()));
                }
                ast::PatternElement::Placeable { expression } => match expression {
                    ast::Expression::Inline(inline) => out.extend(self.inline(inline)?),
                    ast::Expression::Select { selector, variants } => out.push(self.select(selector, variants)?),
                },
            }
        }
        Ok(merge(out))
    }

    fn var(&self, name: &str) -> Result<u8, String> {
        let i = self.vars.iter().position(|v| v == name).ok_or_else(|| self.err(&format!("`${name}` is not in the English")))?;
        u8::try_from(i).map_err(|_| self.err("too many arguments"))
    }

    fn inline(&self, inline: &ast::InlineExpression<&str>) -> Result<Vec<Piece>, String> {
        match inline {
            ast::InlineExpression::StringLiteral { value } => Ok(vec![Piece::Text(unescape(value))]),
            ast::InlineExpression::VariableReference { id } => Ok(vec![Piece::Arg(self.var(id.name)?)]),
            ast::InlineExpression::TermReference { id, attribute: None, arguments: None } => {
                self.terms.get(id.name).cloned().ok_or_else(|| self.err(&format!("no term `-{}`", id.name)))
            }
            _ => Err(self.err("only text, $variables, terms and string literals")),
        }
    }

    fn select(&self, selector: &ast::InlineExpression<&str>, variants: &[ast::Variant<&str>]) -> Result<Piece, String> {
        let arg = match selector {
            ast::InlineExpression::VariableReference { id } => self.var(id.name)?,
            ast::InlineExpression::FunctionReference { id, arguments } if id.name == "NUMBER" => {
                match arguments.positional.as_slice() {
                    [ast::InlineExpression::VariableReference { id }] if arguments.named.is_empty() => self.var(id.name)?,
                    _ => return Err(self.err("NUMBER takes one $variable and no options")),
                }
            }
            _ => return Err(self.err("a selector is a $variable or NUMBER($variable)")),
        };
        let (mut cases, mut other) = (Vec::new(), None);
        for v in variants {
            let ast::VariantKey::Identifier { name } = &v.key else {
                return Err(self.err("variant keys are plural categories, not numbers"));
            };
            plural::variant(name).map_err(|e| self.err(&e))?;
            let body = self.pattern(&v.value)?;
            match (*name, v.default) {
                ("other", true) => other = Some(body),
                (_, true) => return Err(self.err("the default variant is *[other]")),
                ("other", false) => return Err(self.err("[other] is the default, *[other]")),
                (name, false) => cases.push((name.to_string(), body)),
            }
        }
        cases.sort_by_key(|(c, _)| plural::ORDER.iter().position(|o| o == c));
        Ok(Piece::Select { arg, cases, other: other.ok_or_else(|| self.err("no *[other]"))? })
    }
}

fn unescape(s: &str) -> String {
    s.replace("\\\"", "\"").replace("\\\\", "\\")
}

/// Adjacent text joined into one piece.
fn merge(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    for p in pieces {
        match (out.last_mut(), p) {
            (Some(Piece::Text(a)), Piece::Text(b)) => a.push_str(&b),
            (_, p) => out.push(p),
        }
    }
    out.retain(|p| !matches!(p, Piece::Text(t) if t.is_empty()));
    out
}

fn comment(c: Option<&ast::Comment<&str>>) -> Option<String> {
    c.map(|c| c.content.join(" "))
}

/// `vars` names each message's arguments; `None` while reading the source, which sets them.
fn catalogue(path: &Path, vars: &mut BTreeMap<String, Vec<String>>, source: bool) -> Result<Catalogue, String> {
    let text = read(path)?;
    let resource = fluent_syntax::parser::parse(text.as_str())
        .map_err(|(_, errors)| format!("{}: {errors:?}", path.display()))?;
    let mut terms = BTreeMap::new();
    let mut messages = BTreeMap::new();
    for entry in &resource.body {
        match entry {
            ast::Entry::Term(t) => {
                let c = Compiler { id: t.id.name, vars: &[], terms: &terms };
                let pieces = c.pattern(&t.value)?;
                terms.insert(t.id.name.to_string(), pieces);
            }
            ast::Entry::Message(m) => {
                let Some(value) = &m.value else {
                    return Err(format!("message `{}` has no value", m.id.name));
                };
                if !m.attributes.is_empty() {
                    return Err(format!("message `{}`: no attributes yet", m.id.name));
                }
                if source {
                    let mut v = Vec::new();
                    collect_vars(value, &mut v);
                    vars.insert(m.id.name.to_string(), v);
                }
                let Some(v) = vars.get(m.id.name) else {
                    return Err(format!("message `{}` is not in the English", m.id.name));
                };
                let c = Compiler { id: m.id.name, vars: v, terms: &terms };
                messages.insert(m.id.name.to_string(), (c.pattern(value)?, comment(m.comment.as_ref())));
            }
            ast::Entry::Comment(_) | ast::Entry::GroupComment(_) | ast::Entry::ResourceComment(_) => {}
            ast::Entry::Junk { content } => return Err(format!("{}: cannot parse `{}`", path.display(), content.trim())),
        }
    }
    Ok(Catalogue { messages })
}

/// CLDR's names, patterns and plural categories for one locale, from ICU4X's compiled data.
pub struct Cldr {
    pub weekdays: [String; 7],
    pub months: [String; 12],
    pub date: String,
    pub time: String,
}

/// # Errors
/// A tag ICU4X cannot parse or has no data for.
pub fn cldr(tag: &str) -> Result<Cldr, String> {
    let locale: icu::locale::Locale = tag.parse().map_err(|e| format!("{tag}: {e:?}"))?;
    let mut names: FixedCalendarDateTimeNames<Gregorian, fieldsets::MDE> =
        FixedCalendarDateTimeNames::try_new(locale.clone().into()).map_err(|e| format!("{tag}: {e:?}"))?;
    names.include_weekday_names(WeekdayNameLength::Abbreviated).map_err(|e| format!("{tag}: {e:?}"))?;
    names.include_month_names(MonthNameLength::Abbreviated).map_err(|e| format!("{tag}: {e:?}"))?;
    let name = |pattern: &str, d: Date<Gregorian>| -> Result<String, String> {
        let p: DateTimePattern = pattern.parse().map_err(|e| format!("{e:?}"))?;
        names.with_pattern_unchecked(&p).format(&d).try_write_to_string().map(std::borrow::Cow::into_owned).map_err(|e| format!("{tag}: {e:?}"))
    };
    let gregorian = |y, m, d| Date::try_new_gregorian(y, m, d).map_err(|e| format!("{e:?}"));
    // 2026-10-19 was a Monday.
    let mut weekdays: [String; 7] = Default::default();
    for (i, w) in weekdays.iter_mut().enumerate() {
        *w = name("E", gregorian(2026, 10, 19 + u8::try_from(i).unwrap_or(0))?)?;
    }
    let mut months: [String; 12] = Default::default();
    for (i, m) in months.iter_mut().enumerate() {
        *m = name("MMM", gregorian(2026, 1 + u8::try_from(i).unwrap_or(0), 15)?)?;
    }
    let date = FixedCalendarDateTimeFormatter::<Gregorian, _>::try_new(locale.clone().into(), fieldsets::MDE::medium())
        .map_err(|e| format!("{tag}: {e:?}"))?;
    let date = date.format(&gregorian(2026, 10, 23)?).pattern().to_string();
    let h23: icu::locale::Locale = format!("{tag}-u-hc-h23").parse().map_err(|e| format!("{tag}: {e:?}"))?;
    let time = NoCalendarFormatter::try_new(h23.into(), fieldsets::T::hm()).map_err(|e| format!("{tag}: {e:?}"))?;
    let noon = icu::time::Time::try_new(12, 0, 0, 0).map_err(|e| format!("{e:?}"))?;
    let time = time.format(&noon).pattern().to_string();
    Ok(Cldr { weekdays, months, date, time })
}

/// A CLDR pattern's fields and literal text, as `(field, text)`: a field's letters, or `""`
/// with the text.
fn pattern_pieces(pattern: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut chars = pattern.chars().peekable();
    let mut literal = String::new();
    while let Some(c) = chars.next() {
        if c == '\'' {
            if chars.peek() == Some(&'\'') {
                chars.next();
                literal.push('\'');
                continue;
            }
            // Quoted text, where '' is a quote.
            while let Some(q) = chars.next() {
                if q == '\'' {
                    if chars.peek() != Some(&'\'') {
                        break;
                    }
                    chars.next();
                }
                literal.push(q);
            }
        } else if c.is_ascii_alphabetic() {
            if !literal.is_empty() {
                out.push((String::new(), std::mem::take(&mut literal)));
            }
            let mut field = String::from(c);
            while chars.peek() == Some(&c) {
                field.push(c);
                chars.next();
            }
            out.push((field, String::new()));
        } else {
            literal.push(c);
        }
    }
    if !literal.is_empty() {
        out.push((String::new(), literal));
    }
    out
}

fn pieces_rust(tag: &str, kind: &str, pattern: &str, fields: &[(&str, &str)]) -> Result<String, String> {
    let mut out = String::from("&[");
    for (field, text) in pattern_pieces(pattern) {
        if field.is_empty() {
            write!(out, "{kind}::Text({text:?}), ").unwrap();
        } else {
            let (_, v) = fields
                .iter()
                .find(|(f, _)| *f == field)
                .ok_or_else(|| format!("{tag}: `{field}` in `{pattern}` is not drawn yet"))?;
            write!(out, "{kind}::{v}, ").unwrap();
        }
    }
    Ok(format!("{}]", out.trim_end_matches(", ")))
}

fn camel(id: &str) -> String {
    id.split('-').map(|w| w[..1].to_uppercase() + &w[1..]).collect()
}

fn pieces_code(pieces: &[Piece]) -> String {
    let inner: Vec<String> = pieces
        .iter()
        .map(|p| match p {
            Piece::Text(t) => format!("Piece::Text({t:?})"),
            Piece::Arg(i) => format!("Piece::Arg({i})"),
            Piece::Select { arg, cases, other } => {
                let cases: Vec<String> = cases
                    .iter()
                    .map(|(c, body)| format!("({}, {})", plural::variant(c).unwrap_or("Plural::Other"), pieces_code(body)))
                    .collect();
                format!("Piece::Select {{ arg: {arg}, cases: &[{}], other: {} }}", cases.join(", "), pieces_code(other))
            }
        })
        .collect();
    format!("&[{}]", inner.join(", "))
}

fn text_of(pieces: &[Piece], vars: &[String]) -> String {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Text(t) => t.clone(),
            Piece::Arg(i) => format!("{{${}}}", vars[usize::from(*i)]),
            Piece::Select { other, .. } => text_of(other, vars),
        })
        .collect()
}

/// The generated module's text.
///
/// # Errors
/// Anything in `l10n/` outside the subset, a message without an id or the reverse, a pattern
/// field not drawn yet, or a plural rule outside the grammar.
pub fn generate(root: &Path) -> Result<String, String> {
    let ids: Ids = toml::from_str(&read(&root.join("l10n/ids.toml"))?).map_err(|e| format!("ids.toml: {e}"))?;
    let mut vars = BTreeMap::new();
    let source = catalogue(&root.join("l10n").join(SOURCE), &mut vars, true)?;
    for id in source.messages.keys() {
        if !ids.messages.contains_key(id) {
            return Err(format!("message `{id}` has no number in ids.toml"));
        }
    }
    let mut by_number: BTreeMap<u16, &str> = BTreeMap::new();
    for (id, n) in &ids.messages {
        if by_number.insert(*n, id).is_some() {
            return Err(format!("number {n} is used twice in ids.toml"));
        }
    }
    let count = by_number.keys().next_back().map_or(0, |n| usize::from(*n) + 1);

    let mut out = String::from(
        "// Generated by `cargo run -p wrist-l10n-gen` from `l10n/`; edit those, not this.\n\n\
         use crate::{DatePiece, Locale, Piece, Plural, TimePiece};\n\n",
    );
    out.push_str("/// Every message, by its number in `l10n/ids.toml`.\n");
    out.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n#[repr(u16)]\npub enum Msg {\n");
    for (n, id) in &by_number {
        if let Some((pieces, note)) = source.messages.get(*id) {
            if let Some(note) = note {
                writeln!(out, "    /// {note}").unwrap();
                out.push_str("    ///\n");
            }
            writeln!(out, "    /// `{}`", text_of(pieces, &vars[*id])).unwrap();
            writeln!(out, "    {} = {n},", camel(id)).unwrap();
        }
    }
    out.push_str("}\n\n");

    let locales = locales(root)?;
    let mut statics = Vec::new();
    for l in &locales {
        let cat = if l.catalogue == SOURCE { source.messages.clone() } else { catalogue(&root.join("l10n").join(&l.catalogue), &mut vars, false)?.messages };
        let mut c = cldr(&l.tag)?;
        let mut overridden = Vec::new();
        for (month, replacement) in &l.month_overrides {
            let i = month.parse::<usize>().ok().filter(|m| (1..=12).contains(m)).ok_or_else(|| format!("{}: month `{month}`", l.tag))?;
            if c.months[i - 1] == *replacement {
                return Err(format!("{}: month {month} is already {replacement:?} in CLDR; drop the override", l.tag));
            }
            overridden.push(format!("{month}: CLDR {:?}", c.months[i - 1]));
            c.months[i - 1].clone_from(replacement);
        }
        let name = l.tag.replace('-', "_").to_uppercase();
        let rules: Vec<(String, String)> = plural::ORDER
            .iter()
            .filter_map(|cat| l.plural.get(*cat).map(|r| ((*cat).to_string(), r.clone())))
            .collect();
        if let Some(extra) = l.plural.keys().find(|k| !plural::ORDER.contains(&k.as_str()) || k.as_str() == "other") {
            return Err(format!("{}: plural category `{extra}` (other is implied)", l.tag));
        }
        let fn_name = format!("plural_{}", name.to_lowercase());
        writeln!(out, "fn {fn_name}(n: u32) -> Plural {{\n    {}\n}}\n", plural::function_body(&rules)?).unwrap();
        writeln!(out, "/// `{}`, built in.\npub static {name}: Locale = Locale {{", l.tag).unwrap();
        writeln!(out, "    id: {},\n    tag: {:?},\n    messages: &[", l.id, l.tag).unwrap();
        for n in 0..count {
            let id = u16::try_from(n).ok().and_then(|n| by_number.get(&n));
            match id.and_then(|id| cat.get(*id).map(|m| (id, m))) {
                Some((id, (pieces, _))) => writeln!(out, "        // {id}\n        {},", pieces_code(pieces)).unwrap(),
                None => out.push_str("        &[],\n"),
            }
        }
        out.push_str("    ],\n");
        writeln!(out, "    weekdays: {:?},", c.weekdays).unwrap();
        if !overridden.is_empty() {
            writeln!(out, "    // Overridden in locales.toml: {}.", overridden.join(", ")).unwrap();
        }
        writeln!(out, "    months: {:?},", c.months).unwrap();
        writeln!(out, "    // {}", c.date).unwrap();
        writeln!(out, "    date: {},", pieces_rust(&l.tag, "DatePiece", &c.date, &[("E", "Weekday"), ("d", "Day"), ("MMM", "Month")])?).unwrap();
        writeln!(out, "    // {}", c.time).unwrap();
        writeln!(out, "    time: {},", pieces_rust(&l.tag, "TimePiece", &c.time, &[("HH", "Hour"), ("mm", "Minute")])?).unwrap();
        writeln!(out, "    plural: {fn_name},\n}};\n").unwrap();
        statics.push(name);
    }
    writeln!(out, "/// Every built-in locale.\npub static BUILT_IN: [&Locale; {}] = [{}];", statics.len(), statics.iter().map(|s| format!("&{s}")).collect::<Vec<_>>().join(", ")).unwrap();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_split_into_fields_and_text() {
        let p = pattern_pieces("E, d. MMM 'o''clock'");
        let fields: Vec<&str> = p.iter().map(|(f, t)| if f.is_empty() { t.as_str() } else { f.as_str() }).collect();
        assert_eq!(fields, ["E", ", ", "d", ". ", "MMM", " o'clock"]);
    }

    #[test]
    fn a_field_not_drawn_yet_is_refused() {
        let e = pieces_rust("xx", "DatePiece", "EEEE d MMM", &[("E", "Weekday"), ("d", "Day"), ("MMM", "Month")]).unwrap_err();
        assert!(e.contains("`EEEE`"), "{e}");
    }

    #[test]
    fn ids_become_variants() {
        assert_eq!(camel("battery-cell-mv"), "BatteryCellMv");
    }

    fn compile(ftl: &str) -> Result<Catalogue, String> {
        let dir = std::env::temp_dir().join(format!("wrist-l10n-gen-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.ftl");
        fs::write(&path, ftl).unwrap();
        catalogue(&path, &mut BTreeMap::new(), true)
    }

    #[test]
    fn the_subset_compiles() {
        let c = compile("-p = wrist-rs\nm = { -p } has { $n ->\n    [one] one step\n   *[other] { $n } steps\n} { \"{\" }\n").unwrap();
        let (pieces, _) = &c.messages["m"];
        assert_eq!(pieces[0], Piece::Text("wrist-rs has ".into()));
        let Piece::Select { arg: 0, cases, other } = &pieces[1] else { panic!("{pieces:?}") };
        assert_eq!(cases[0].0, "one");
        assert_eq!(other, &[Piece::Arg(0), Piece::Text(" steps".into())]);
        assert_eq!(pieces[2], Piece::Text(" {".into()));
    }

    #[test]
    fn outside_the_subset_is_refused() {
        for (ftl, why) in [
            ("m = { $n ->\n   *[one] a\n    [other] b\n}\n", "*[other]"),
            ("m = { $n ->\n    [0] a\n   *[other] b\n}\n", "plural categories"),
            ("m = { -missing }\n", "no term"),
            ("m = a\n    .attr = b\n", "attributes"),
            ("m = { NUMBER($n, minimumFractionDigits: 2) }\n", "only text"),
        ] {
            let e = compile(ftl).err().unwrap_or_default();
            assert!(e.contains(why), "{ftl:?}: {e}");
        }
    }
}
