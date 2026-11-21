//! `.mayu` の字句解析。yamy の `Parser::getLine` を写している。
//!
//! - 1 行 = 1 文。`#` 以降はコメント
//! - コメントの外で行の最後の文字が `\` なら次の行に続く（コメントの中の `\` では続かない）
//! - 記号・接頭辞（`=` `+=` `C-` `E0-` `*` `~` など）は最長一致で 1 トークン
//! - `(` か `,` の直後に `,` `)` が来たら、間に空の名前を入れる（空の引数）
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
    "~",
];

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || !c.is_ascii() || "-+/?_\\".contains(c)
}

/// ファイル全体を文の列にする。
pub fn tokenize(src: &str) -> Result<Vec<Line>, LexError> {
    let mut out = Vec::new();
    let mut lines = src.lines().enumerate();
    while let Some((idx, first)) = lines.next() {
        let start = idx + 1;
        let mut tokens = Vec::new();
        // yamy の `isTokenExist`。`(` か `,` の後に語が来たか。継続行をまたいで引き継ぐ
        let mut token_exist = false;
        let mut text = first;
        while tokenize_line(text, start, &mut tokens, &mut token_exist)? {
            match lines.next() {
                Some((_, next)) => text = next,
                None => break,
            }
        }
        if !tokens.is_empty() {
            out.push(Line {
                line: start,
                tokens,
            });
        }
    }
    Ok(out)
}

/// 1 行の字句を `tokens` に足す。次の行に続くなら `true`。
fn tokenize_line(
    text: &str,
    line: usize,
    tokens: &mut Vec<Token>,
    token_exist: &mut bool,
) -> Result<bool, LexError> {
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        let Some(c) = rest.chars().next() else { break };
        if c == '#' {
            break;
        }
        if rest == "\\" {
            return Ok(true);
        }

        // 空の引数：`(` か `,` の直後の `,` `)` の前に空の名前を入れる（`()` も空の引数 1 個）
        if c == ',' || c == '(' || c == ')' {
            if c != '(' && !*token_exist {
                tokens.push(Token::Name(String::new()));
            }
            *token_exist = c == ')';
            tokens.push(Token::Sym(match c {
                ',' => ",",
                '(' => "(",
                _ => ")",
            }));
            rest = &rest[1..];
            continue;
        }
        *token_exist = true;

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

        // 名前または数値。`\` はその次の文字ごと名前に含める。行の最後の `\` は含めない（継続）
        let mut len = 0;
        let mut chars = rest.char_indices().peekable();
        while let Some((_, ch)) = chars.next() {
            if !is_name_char(ch) {
                break;
            }
            if ch == '\\' {
                if chars.peek().is_none() {
                    break;
                }
                chars.next();
            }
            len = chars.peek().map_or(rest.len(), |(j, _)| *j);
        }
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
    Ok(false)
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
    fn empty_argument_becomes_empty_name() {
        let f = |args: Vec<Token>| {
            let mut v = vec![Token::Sym("&"), Token::Name("F".into()), Token::Sym("(")];
            v.extend(args);
            v.push(Token::Sym(")"));
            v
        };
        let empty = || Token::Name(String::new());
        assert_eq!(
            toks("&F(\"a\", )"),
            f(vec![Token::Str("a".into()), Token::Sym(","), empty()])
        );
        assert_eq!(toks("&F()"), f(vec![empty()]));
        assert_eq!(
            toks("&F(1,, 2)"),
            f(vec![
                Token::Number(1, "1".into()),
                Token::Sym(","),
                empty(),
                Token::Sym(","),
                Token::Number(2, "2".into()),
            ])
        );
        // 行継続をまたいでも同じ
        assert_eq!(
            toks("&F(\"a\", \\\n )"),
            f(vec![Token::Str("a".into()), Token::Sym(","), empty()])
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
    fn backslash_in_comment_does_not_continue() {
        // 109.mayu の `keyseq $REVERSE_SOLIDUS … # \`。次の行は別の文
        let lines = tokenize("keyseq $A = B # \\\nkeyseq $C = D\n").unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].line, 2);
        assert_eq!(lines[1].tokens[0], Token::Name("keyseq".into()));
    }

    #[test]
    fn backslash_after_name_continues() {
        assert_eq!(
            toks("def key Foo\\\n= 0x10"),
            vec![
                Token::Name("def".into()),
                Token::Name("key".into()),
                Token::Name("Foo".into()),
                Token::Sym("="),
                Token::Number(0x10, "0x10".into()),
            ]
        );
        // `\\` は 1 つ目が 2 つ目を名前に取り込むので続かない
        let lines = tokenize("key A = B\\\\\nkey C = D").unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].tokens[3], Token::Name("B\\\\".into()));
        // 後ろに空白があれば行の最後ではないので続かない
        let lines = tokenize("key A = B \\ \nkey C = D").unwrap();
        assert_eq!(lines.len(), 2);
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
