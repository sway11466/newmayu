//! `.mayu` の字句解析。yamy の `Parser::getLine` を写している。
//!
//! - 1 行 = 1 文。行末の `\` で次の行に続く。`#` 以降はコメント
//! - 記号・接頭辞（`=` `+=` `C-` `E0-` `*` `~` など）は最長一致で 1 トークン
//! - `"..."` `'...'` は文字列、`/.../` は正規表現
//! - それ以外は名前。名前に使える文字は英数字・非 ASCII・`-+/?_\`
//! - 名前が数値として読めれば数値（`0x3a` `45`）。読めた所までで区切る
//! - 大文字小文字は区別しない（比較は呼び元が `eq_ignore_ascii_case` で行う）

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// 名前（キー名・キーマップ名・キーワード）。引用符なし。
    Name(String),
    /// `"..."` `'...'`。
    Str(String),
    /// `/.../`。
    Regex(String),
    /// 数値。元の表記も残す（エラー表示用）。
    Number(i64, String),
    /// `=` `+=` `C-` `*` `(` `)` `,` など。表記そのまま（大文字に正規化）。
    Sym(&'static str),
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Name(s) => write!(f, "{s}"),
            Token::Str(s) => write!(f, "\"{s}\""),
            Token::Regex(s) => write!(f, "/{s}/"),
            Token::Number(_, s) => write!(f, "{s}"),
            Token::Sym(s) => write!(f, "{s}"),
        }
    }
}

/// 1 文（継続行を結合したもの）。`line` は先頭行の行番号（1 始まり）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub line: usize,
    pub tokens: Vec<Token>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.line, self.message)
    }
}

/// 記号と接頭辞。長いものから試す（`=>` より先に `=` を取らないため）。
/// yamy の `prefixes[]` と同じ集合。
const SYMBOLS: &[&str] = &[
    "!!!", "MMAX-", "MMIN-", "MAX-", "MIN-", "=>", "&&", "||", "-=", "+=", "!!", "E0-", "E1-",
    "S-", "A-", "M-", "C-", "W-", "U-", "D-", "R-", "IL-", "IC-", "I-", "NL-", "CL-", "SL-", "KL-",
    "T-", "TS-", "M0-", "M1-", "M2-", "M3-", "M4-", "M5-", "M6-", "M7-", "M8-", "M9-", "L0-",
    "L1-", "L2-", "L3-", "L4-", "L5-", "L6-", "L7-", "L8-", "L9-", "=", ":", "$", "&", "!", "*",
    "~", "(", ")", ",",
];

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || !c.is_ascii() || "-+/?_\\".contains(c)
}

/// ファイル全体を文の列にする。
pub fn tokenize(src: &str) -> Result<Vec<Line>, LexError> {
    let mut out = Vec::new();
    let mut lines = src.lines().enumerate().peekable();
    while let Some((idx, first)) = lines.next() {
        let start = idx + 1;
        // 行末 `\` で継続
        let mut text = first.to_string();
        while text.trim_end().ends_with('\\') && !text.trim_end().ends_with("\\\\") {
            let trimmed = text.trim_end();
            text = trimmed[..trimmed.len() - 1].to_string();
            match lines.next() {
                Some((_, next)) => {
                    text.push(' ');
                    text.push_str(next);
                }
                None => break,
            }
        }
        let tokens = tokenize_line(&text, start)?;
        if !tokens.is_empty() {
            out.push(Line {
                line: start,
                tokens,
            });
        }
    }
    Ok(out)
}

fn tokenize_line(text: &str, line: usize) -> Result<Vec<Token>, LexError> {
    let mut tokens = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        let Some(c) = rest.chars().next() else { break };
        if c == '#' {
            break;
        }

        // 記号・接頭辞（大文字小文字を問わない）
        if let Some(sym) = SYMBOLS.iter().find(|s| starts_with_ignore_case(rest, s)) {
            // `-` を含む接頭辞は、直後が名前の続きでなければ接頭辞として扱う。
            // `C-` の後は必ずキー名か別の接頭辞が来るので区別は要らない。
            tokens.push(Token::Sym(sym));
            rest = &rest[sym.len()..];
            continue;
        }

        // 文字列・正規表現
        if c == '"' || c == '\'' || c == '/' {
            let body = &rest[1..];
            let mut end = None;
            let mut chars = body.char_indices();
            while let Some((i, ch)) = chars.next() {
                if ch == '\\' {
                    chars.next();
                    continue;
                }
                if ch == c {
                    end = Some(i);
                    break;
                }
            }
            let Some(end) = end else {
                return Err(LexError {
                    line,
                    message: format!("閉じる {c} が無い"),
                });
            };
            let s = unescape(&body[..end]);
            tokens.push(if c == '/' {
                Token::Regex(s)
            } else {
                Token::Str(s)
            });
            rest = &body[end + 1..];
            continue;
        }

        // 名前または数値
        let len = rest
            .char_indices()
            .find(|(_, ch)| !is_name_char(*ch))
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        if len == 0 {
            return Err(LexError {
                line,
                message: format!("使えない文字 {c:?}"),
            });
        }
        let word = &rest[..len];
        if let Some((value, used)) = parse_number_prefix(word) {
            tokens.push(Token::Number(value, word[..used].to_string()));
            rest = &rest[used..];
        } else {
            tokens.push(Token::Name(word.to_string()));
            rest = &rest[len..];
        }
    }
    Ok(tokens)
}

fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len()
        && s.is_char_boundary(prefix.len())
        && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// C の `strtol(s, &end, 0)` と同じ読み方。先頭が数値なら (値, 消費した長さ)。
fn parse_number_prefix(s: &str) -> Option<(i64, usize)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut neg = false;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        neg = bytes[i] == b'-';
        i += 1;
    }
    let (radix, digits_start) = if bytes.len() > i + 1
        && bytes[i] == b'0'
        && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X')
        && bytes.get(i + 2).is_some_and(|b| b.is_ascii_hexdigit())
    {
        (16, i + 2)
    } else if bytes.len() > i && bytes[i] == b'0' {
        (8, i)
    } else {
        (10, i)
    };
    let mut end = digits_start;
    while end < bytes.len() && (bytes[end] as char).is_digit(radix) {
        end += 1;
    }
    if end == digits_start {
        return None;
    }
    let value = i64::from_str_radix(&s[digits_start..end], radix).ok()?;
    Some((if neg { -value } else { value }, end))
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Token> {
        let lines = tokenize(s).unwrap();
        assert_eq!(lines.len(), 1, "{lines:?}");
        lines.into_iter().next().unwrap().tokens
    }

    #[test]
    fn def_key_with_scan_code_and_comment() {
        assert_eq!(
            toks("def key HyphenMinus Hyphen Minus\t=    0x0c # -="),
            vec![
                Token::Name("def".into()),
                Token::Name("key".into()),
                Token::Name("HyphenMinus".into()),
                Token::Name("Hyphen".into()),
                Token::Name("Minus".into()),
                Token::Sym("="),
                Token::Number(0x0c, "0x0c".into()),
            ]
        );
    }

    #[test]
    fn prefixes_and_japanese_names() {
        assert_eq!(
            toks(" key *IC-C-Yen = $ToggleIME"),
            vec![
                Token::Name("key".into()),
                Token::Sym("*"),
                Token::Sym("IC-"),
                Token::Sym("C-"),
                Token::Name("Yen".into()),
                Token::Sym("="),
                Token::Sym("$"),
                Token::Name("ToggleIME".into()),
            ]
        );
        assert_eq!(
            toks("key *E0半角/全角 = *Esc"),
            vec![
                Token::Name("key".into()),
                Token::Sym("*"),
                // `E0半角/全角` は `E0-` 接頭辞ではなく、そういう名前のキー
                Token::Name("E0半角/全角".into()),
                Token::Sym("="),
                Token::Sym("*"),
                Token::Name("Esc".into()),
            ]
        );
    }

    #[test]
    fn function_with_arguments() {
        assert_eq!(
            toks("key C-S-Left = &WindowMove(-16, 0)"),
            vec![
                Token::Name("key".into()),
                Token::Sym("C-"),
                Token::Sym("S-"),
                Token::Name("Left".into()),
                Token::Sym("="),
                Token::Sym("&"),
                Token::Name("WindowMove".into()),
                Token::Sym("("),
                Token::Number(-16, "-16".into()),
                Token::Sym(","),
                Token::Number(0, "0".into()),
                Token::Sym(")"),
            ]
        );
    }

    #[test]
    fn continuation_and_strings() {
        let lines = tokenize("def key Foo \\\n\t= E0-0x10 # c\n\ninclude \"109.mayu\"\n").unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].line, 1);
        assert_eq!(
            lines[0].tokens,
            vec![
                Token::Name("def".into()),
                Token::Name("key".into()),
                Token::Name("Foo".into()),
                Token::Sym("="),
                Token::Sym("E0-"),
                Token::Number(0x10, "0x10".into()),
            ]
        );
        assert_eq!(lines[1].line, 4);
        assert_eq!(
            lines[1].tokens,
            vec![Token::Name("include".into()), Token::Str("109.mayu".into())]
        );
    }

    #[test]
    fn regex_token() {
        assert_eq!(
            toks("window EditControl /:(Edit|TEdit)$/ : EmacsEdit"),
            vec![
                Token::Name("window".into()),
                Token::Name("EditControl".into()),
                Token::Regex(":(Edit|TEdit)$".into()),
                Token::Sym(":"),
                Token::Name("EmacsEdit".into()),
            ]
        );
    }
}
