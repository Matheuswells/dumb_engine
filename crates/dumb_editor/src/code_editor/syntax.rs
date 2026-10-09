//! Instant syntax check of a Rust buffer (no rust-analyzer needed): parses with `syn` and
//! reports the first error with its exact position.

/// A diagnostic with a precise range (0-based lines, columns in chars).
#[derive(Clone, Debug, PartialEq)]
pub struct LiveDiag {
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub error: bool,
    pub text: String,
    /// "syntax", "rust-analyzer", ...
    pub source: &'static str,
}

pub fn check(src: &str) -> Vec<LiveDiag> {
    match syn::parse_file(src) {
        Ok(_) => Vec::new(),
        Err(e) => e
            .into_iter()
            .map(|e| {
                let (s, end) = (e.span().start(), e.span().end());
                let line = s.line.saturating_sub(1);
                let mut end_line = end.line.saturating_sub(1);
                let mut end_col = end.column;
                // Errors at end of input or zero-width: underline at least one character.
                if end_line < line || (end_line == line && end_col <= s.column) {
                    end_line = line;
                    end_col = s.column + 1;
                }
                // syn says what it expected; add what it found, like rustc does.
                let found: String = src.lines().nth(line).map(|l| l.chars().skip(s.column).take_while(|c| !c.is_whitespace()).take(24).collect()).unwrap_or_default();
                let text = if found.is_empty() { format!("{e} (found end of file)") } else { format!("{e}, found `{found}`") };
                LiveDiag { line, col: s.column, end_line, end_col, error: true, text, source: "syntax" }
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_stray_tokens() {
        let src = "struct Move { speed: f32 }\n\nimpl Default for Move {aaaa\n    fn default() -> Self { Move { speed: 1.0 } }\n}\n";
        let d = check(src);
        assert_eq!(d.len(), 1, "{d:?}");
        // Like rustc, the parser fails at the next token: `aaaa` could start a macro call.
        assert_eq!((d[0].line, d[0].col), (3, 4), "{d:?}");
        assert!(d[0].text.contains('!'));
        assert!(check("fn main() { let x = 1; }").is_empty());
    }

    #[test]
    fn unclosed_brace() {
        let d = check("fn main() {\n    let x = 1;\n");
        assert_eq!(d.len(), 1);
    }
}
