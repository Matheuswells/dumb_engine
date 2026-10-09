//! Rust syntax highlighting for the code editor (a small hand-written tokenizer).

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    Plain,
    Keyword,
    Control,
    Type,
    Function,
    Macro,
    Str,
    Char,
    Number,
    Comment,
    DocComment,
    Attribute,
    Lifetime,
    Punct,
}

const KEYWORDS: &[&str] = &[
    "as", "const", "crate", "dyn", "enum", "extern", "false", "fn", "impl", "in", "let", "mod", "move", "mut", "pub", "ref", "self", "Self", "static", "struct",
    "super", "trait", "true", "type", "unsafe", "use", "where", "async", "await", "union",
];
const CONTROL: &[&str] = &["break", "continue", "else", "for", "if", "loop", "match", "return", "while", "yield"];
const PRIMITIVES: &[&str] = &["bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64"];

/// Split source into (byte range, token kind) spans covering the whole text.
pub fn tokenize(src: &str) -> Vec<(std::ops::Range<usize>, Tok)> {
    let b = src.as_bytes();
    let n = b.len();
    let mut out: Vec<(std::ops::Range<usize>, Tok)> = Vec::new();
    let push = |r: std::ops::Range<usize>, t: Tok, out: &mut Vec<(std::ops::Range<usize>, Tok)>| {
        if r.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.1 == t && last.0.end == r.start => last.0.end = r.end,
            _ => out.push((r, t)),
        }
    };
    let mut i = 0;
    while i < n {
        let c = b[i];
        let start = i;
        // Comments.
        if c == b'/' && i + 1 < n && b[i + 1] == b'/' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            let doc = src[start..i].starts_with("///") || src[start..i].starts_with("//!");
            push(start..i, if doc { Tok::DocComment } else { Tok::Comment }, &mut out);
            continue;
        }
        if c == b'/' && i + 1 < n && b[i + 1] == b'*' {
            let mut depth = 0;
            while i < n {
                if b[i] == b'/' && i + 1 < n && b[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && i + 1 < n && b[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            push(start..i.min(n), Tok::Comment, &mut out);
            continue;
        }
        // Raw strings r"..." r#"..."# (and br"...").
        let raw_at = if c == b'r' { Some(i + 1) } else if c == b'b' && i + 1 < n && b[i + 1] == b'r' { Some(i + 2) } else { None };
        if let Some(mut j) = raw_at {
            let mut hashes = 0;
            while j < n && b[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < n && b[j] == b'"' && (hashes > 0 || raw_at == Some(i + 1) || raw_at == Some(i + 2)) {
                j += 1;
                loop {
                    if j >= n {
                        break;
                    }
                    if b[j] == b'"' && b[j + 1..].iter().take(hashes).filter(|x| **x == b'#').count() == hashes && j + hashes < n + 1 {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                i = j.min(n);
                push(start..i, Tok::Str, &mut out);
                continue;
            }
        }
        // Strings.
        if c == b'"' || (c == b'b' && i + 1 < n && b[i + 1] == b'"') {
            i += if c == b'b' { 2 } else { 1 };
            while i < n && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(n);
            push(start..i, Tok::Str, &mut out);
            continue;
        }
        // Chars vs lifetimes.
        if c == b'\'' {
            if i + 2 < n && b[i + 1] == b'\\' {
                let mut j = i + 2;
                while j < n && b[j] != b'\'' && b[j] != b'\n' {
                    j += 1;
                }
                i = (j + 1).min(n);
                push(start..i, Tok::Char, &mut out);
                continue;
            }
            // 'x' (one char, possibly multi-byte)
            if let Some(ch) = src[i + 1..].chars().next() {
                let after = i + 1 + ch.len_utf8();
                if after < n && b[after] == b'\'' {
                    i = after + 1;
                    push(start..i, Tok::Char, &mut out);
                    continue;
                }
            }
            i += 1;
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            push(start..i, Tok::Lifetime, &mut out);
            continue;
        }
        // Attributes #[...] / #![...]
        if c == b'#' && i + 1 < n && (b[i + 1] == b'[' || (b[i + 1] == b'!' && i + 2 < n && b[i + 2] == b'[')) {
            let mut depth = 0;
            while i < n {
                if b[i] == b'[' {
                    depth += 1;
                } else if b[i] == b']' {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                } else if b[i] == b'\n' && depth == 0 {
                    break;
                }
                i += 1;
            }
            push(start..i, Tok::Attribute, &mut out);
            continue;
        }
        // Numbers.
        if c.is_ascii_digit() {
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || (b[i] == b'.' && i + 1 < n && b[i + 1].is_ascii_digit())) {
                i += 1;
            }
            push(start..i, Tok::Number, &mut out);
            continue;
        }
        // Identifiers.
        if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80) {
                i += 1;
            }
            // Don't split UTF-8 sequences.
            while i < n && !src.is_char_boundary(i) {
                i += 1;
            }
            let word = &src[start..i];
            let next = src[i..].trim_start_matches([' ', '\t']).as_bytes().first().copied();
            let t = if CONTROL.contains(&word) {
                Tok::Control
            } else if KEYWORDS.contains(&word) {
                Tok::Keyword
            } else if next == Some(b'!') && !src[i..].starts_with("!=") {
                Tok::Macro
            } else if PRIMITIVES.contains(&word) || word.chars().next().is_some_and(|ch| ch.is_uppercase()) {
                Tok::Type
            } else if next == Some(b'(') || src[i..].starts_with("::<") {
                Tok::Function
            } else {
                Tok::Plain
            };
            push(start..i, t, &mut out);
            if t == Tok::Macro {
                // include the `!`
                let bang = start + word.len() + src[i..].find('!').unwrap_or(0);
                push(i..bang + 1, Tok::Macro, &mut out);
                i = bang + 1;
            }
            continue;
        }
        // Everything else, one char at a time (keep UTF-8 intact).
        let len = src[i..].chars().next().map_or(1, |ch| ch.len_utf8());
        i += len;
        let t = if c.is_ascii_punctuation() { Tok::Punct } else { Tok::Plain };
        push(start..i, t, &mut out);
    }
    out
}

pub struct Palette {
    pub plain: Color32,
    pub keyword: Color32,
    pub control: Color32,
    pub ty: Color32,
    pub function: Color32,
    pub macro_: Color32,
    pub string: Color32,
    pub number: Color32,
    pub comment: Color32,
    pub doc: Color32,
    pub attribute: Color32,
    pub lifetime: Color32,
    pub punct: Color32,
}

impl Palette {
    pub fn for_visuals(v: &egui::Visuals) -> Self {
        if v.dark_mode {
            Palette {
                plain: Color32::from_rgb(212, 212, 212),
                keyword: Color32::from_rgb(86, 156, 214),
                control: Color32::from_rgb(197, 134, 192),
                ty: Color32::from_rgb(78, 201, 176),
                function: Color32::from_rgb(220, 220, 170),
                macro_: Color32::from_rgb(79, 193, 255),
                string: Color32::from_rgb(206, 145, 120),
                number: Color32::from_rgb(181, 206, 168),
                comment: Color32::from_rgb(106, 153, 85),
                doc: Color32::from_rgb(120, 170, 100),
                attribute: Color32::from_rgb(156, 160, 175),
                lifetime: Color32::from_rgb(86, 156, 214),
                punct: Color32::from_rgb(180, 180, 180),
            }
        } else {
            Palette {
                plain: Color32::from_rgb(30, 30, 30),
                keyword: Color32::from_rgb(0, 0, 200),
                control: Color32::from_rgb(160, 30, 160),
                ty: Color32::from_rgb(38, 127, 153),
                function: Color32::from_rgb(121, 94, 38),
                macro_: Color32::from_rgb(0, 112, 193),
                string: Color32::from_rgb(163, 21, 21),
                number: Color32::from_rgb(9, 134, 88),
                comment: Color32::from_rgb(0, 128, 0),
                doc: Color32::from_rgb(0, 110, 0),
                attribute: Color32::from_rgb(110, 110, 120),
                lifetime: Color32::from_rgb(0, 0, 200),
                punct: Color32::from_rgb(70, 70, 70),
            }
        }
    }

    fn color(&self, t: Tok) -> Color32 {
        match t {
            Tok::Plain => self.plain,
            Tok::Keyword => self.keyword,
            Tok::Control => self.control,
            Tok::Type => self.ty,
            Tok::Function => self.function,
            Tok::Macro => self.macro_,
            Tok::Str | Tok::Char => self.string,
            Tok::Number => self.number,
            Tok::Comment => self.comment,
            Tok::DocComment => self.doc,
            Tok::Attribute => self.attribute,
            Tok::Lifetime => self.lifetime,
            Tok::Punct => self.punct,
        }
    }
}

/// Build a highlighted layout. `marks` are byte ranges painted with a background (search hits).
pub fn layout(src: &str, font: FontId, pal: &Palette, marks: &[(std::ops::Range<usize>, Color32)], is_rust: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let spans = if is_rust { tokenize(src) } else { vec![(0..src.len(), Tok::Plain)] };
    for (r, t) in spans {
        // Split spans at mark boundaries so marks get their own background.
        let mut cuts = vec![r.start, r.end];
        for (m, _) in marks {
            for p in [m.start, m.end] {
                if p > r.start && p < r.end && src.is_char_boundary(p) {
                    cuts.push(p);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let bg = marks.iter().find(|(m, _)| m.start <= a && b <= m.end).map_or(Color32::TRANSPARENT, |(_, c)| *c);
            let mut fmt = TextFormat::simple(font.clone(), pal.color(t));
            fmt.background = bg;
            if matches!(t, Tok::DocComment) {
                fmt.italics = true;
            }
            job.append(&src[a..b], 0.0, fmt);
        }
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(&str, Tok)> {
        tokenize(src).into_iter().filter(|(_, t)| *t != Tok::Plain && *t != Tok::Punct).map(|(r, t)| (&src[r], t)).collect()
    }

    #[test]
    fn covers_everything() {
        let src = "fn main() { let s = \"a\\\"b\"; // hi\n let c = 'x'; let l: &'static str = r#\"raw\"#; println!(\"{}\", 1.5e3); } /* a /* b */ c */ é";
        let toks = tokenize(src);
        let mut pos = 0;
        for (r, _) in &toks {
            assert_eq!(r.start, pos);
            pos = r.end;
        }
        assert_eq!(pos, src.len());
    }

    #[test]
    fn classifies() {
        let k = kinds("#[derive(Debug)]\npub struct Foo<'a> { x: u32 }\nfn go() { if x { bar(1); vec![2]; } } // c\n/// doc");
        assert!(k.contains(&("#[derive(Debug)]", Tok::Attribute)));
        assert!(k.contains(&("pub", Tok::Keyword)));
        assert!(k.contains(&("Foo", Tok::Type)));
        assert!(k.contains(&("'a", Tok::Lifetime)));
        assert!(k.contains(&("u32", Tok::Type)));
        assert!(k.contains(&("if", Tok::Control)));
        assert!(k.contains(&("bar", Tok::Function)));
        assert!(k.contains(&("vec!", Tok::Macro)));
        assert!(k.contains(&("1", Tok::Number)));
        assert!(k.contains(&("// c", Tok::Comment)));
        assert!(k.contains(&("/// doc", Tok::DocComment)));
    }

    #[test]
    fn strings_and_chars() {
        let k = kinds("let a = 'x'; let b = '\\n'; let c = \"it's\"; let d = r#\"q\"q\"#;");
        assert!(k.contains(&("'x'", Tok::Char)));
        assert!(k.contains(&("'\\n'", Tok::Char)));
        assert!(k.contains(&("\"it's\"", Tok::Str)));
        assert!(k.contains(&("r#\"q\"q\"#", Tok::Str)), "{k:?}");
    }
}
