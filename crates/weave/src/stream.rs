//! Decide how much of an incoming source buffer is safe to execute.
//!
//! A statement is complete when it ends in a real newline and is not an open
//! `if`/`for` suite. Compound statements stay buffered until a later line at
//! indent 0 closes them, or until the caller finishes the program.

use ruff_python_ast::token::TokenKind;
use ruff_python_parser::lexer::lex;
use ruff_python_parser::Mode;
use ruff_text_size::TextSize;

#[derive(Clone, Debug)]
struct Tok {
    kind: TokenKind,
    start: usize,
    end: usize,
}

/// Byte length of the longest closed prefix of `src`.
pub fn complete_prefix_end(src: &str) -> usize {
    if let Some(toks) = tokenize(src) {
        return closed_end(&toks);
    }
    let mut best = 0usize;
    for (i, byte) in src.bytes().enumerate() {
        if byte != b'\n' {
            continue;
        }
        let end = i + 1;
        if let Some(toks) = tokenize(&src[..end]) {
            let closed = closed_end(&toks);
            if closed > best {
                best = closed;
            }
        }
    }
    best
}

#[cfg(test)]
pub fn split_complete(src: &str) -> (&str, &str) {
    let end = complete_prefix_end(src);
    (&src[..end], &src[end..])
}

fn tokenize(src: &str) -> Option<Vec<Tok>> {
    let mut lexer = lex(src, Mode::Module);
    let mut out = Vec::new();
    loop {
        let kind = lexer.next_token();
        let range = lexer.current_range();
        let start = u32::from(range.start()) as usize;
        let end = u32::from(range.end()) as usize;
        if kind == TokenKind::Unknown {
            return None;
        }
        out.push(Tok { kind, start, end });
        if kind == TokenKind::EndOfFile {
            break;
        }
    }
    let _ = TextSize::new(0);
    Some(out)
}

fn next_significant(toks: &[Tok], index: usize) -> Option<&Tok> {
    toks.iter().skip(index + 1).find(|tok| {
        !matches!(
            tok.kind,
            TokenKind::Comment | TokenKind::NonLogicalNewline
        )
    })
}

fn is_clause(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Else | TokenKind::Elif | TokenKind::Except | TokenKind::Finally
    )
}

fn closed_end(toks: &[Tok]) -> usize {
    let mut nesting: i32 = 0;
    let mut indent: i32 = 0;
    let mut last_completed = 0usize;
    let mut prev_kind: Option<TokenKind> = None;
    for (i, tok) in toks.iter().enumerate() {
        let before = prev_kind;
        if !matches!(tok.kind, TokenKind::Comment | TokenKind::NonLogicalNewline) {
            prev_kind = Some(tok.kind);
        }
        match tok.kind {
            TokenKind::Lpar | TokenKind::Lsqb | TokenKind::Lbrace => nesting += 1,
            TokenKind::Rpar | TokenKind::Rsqb | TokenKind::Rbrace => nesting -= 1,
            TokenKind::Indent => indent += 1,
            TokenKind::Dedent => {
                indent -= 1;
                let next = next_significant(toks, i);
                let synthetic = matches!(next.map(|t| t.kind), Some(TokenKind::EndOfFile) | None);
                let clause = matches!(next.map(|t| t.kind), Some(k) if is_clause(k));
                // `el` may still become `else`. Only trust the next token once
                // something real follows it on the stream.
                let settled = next.is_some_and(|n| {
                    toks.iter().any(|t| {
                        t.start >= n.end
                            && t.start != t.end
                            && !matches!(t.kind, TokenKind::EndOfFile)
                    })
                });
                if indent == 0 && !synthetic && !clause && settled {
                    // The compound statement ended where the next line begins.
                    last_completed = tok.start;
                }
            }
            TokenKind::Newline if nesting <= 0 => {
                let real_newline = tok.start != tok.end;
                if real_newline && indent == 0 {
                    let next = next_significant(toks, i).map(|t| t.kind);
                    // A header's newline is followed by Indent. The suite is still open.
                    // A header whose body has not arrived yet ends in `:` with no Indent.
                    let open_header = before == Some(TokenKind::Colon);
                    if !matches!(next, Some(TokenKind::Indent)) && !open_header {
                        last_completed = tok.end;
                    }
                }
            }
            _ => {}
        }
    }
    last_completed
}

#[cfg(test)]
mod tests {
    use super::split_complete;

    #[test]
    fn simple_statement_needs_newline() {
        assert_eq!(split_complete("a = 1"), ("", "a = 1"));
        assert_eq!(split_complete("a = 1\n"), ("a = 1\n", ""));
    }

    #[test]
    fn open_if_stays_buffered() {
        let src = "if cond:\n    a = 1\n";
        assert_eq!(split_complete(src), ("", src));
    }

    #[test]
    fn statement_before_open_if_is_released() {
        let src = "a = 1\nif cond:\n    b = 2\n";
        assert_eq!(split_complete(src), ("a = 1\n", "if cond:\n    b = 2\n"));
    }

    #[test]
    fn full_program_including_if_is_complete() {
        let src = "a = 1\nif cond:\n    b = 2\nc = 3\n";
        assert_eq!(split_complete(src), (src, ""));
    }

    #[test]
    fn else_keeps_if_open() {
        let src = "if cond:\n    a = 1\nelse:\n    b = 2\n";
        assert_eq!(split_complete(src), ("", src));
    }

    #[test]
    fn else_program_is_complete_once_closed() {
        let src = "if cond:\n    a = 1\nelse:\n    b = 2\nc = 3\n";
        assert_eq!(split_complete(src), (src, ""));
    }

    #[test]
    fn parentheses_hold_the_statement() {
        assert_eq!(split_complete("a = (\n  1\n"), ("", "a = (\n  1\n"));
        assert_eq!(split_complete("a = (\n  1\n)\n"), ("a = (\n  1\n)\n", ""));
    }

    #[test]
    fn fstring_with_inner_quotes() {
        let src = "x = f\"value {obj[\"key\"]}\"\n";
        assert_eq!(split_complete(src), (src, ""));
    }

    #[test]
    fn header_without_body_stays_open() {
        let src = "a = 1\nif cond:\n";
        assert_eq!(split_complete(src), ("a = 1\n", "if cond:\n"));
        let src = "a = 1\nfor i in range(3):\n";
        assert_eq!(split_complete(src), ("a = 1\n", "for i in range(3):\n"));
    }

    #[test]
    fn partial_else_keyword_keeps_if_open() {
        let src = "if a:\n    b = 1\nel";
        assert_eq!(split_complete(src), ("", src));
        let src = "if a:\n    b = 1\nelse";
        assert_eq!(split_complete(src), ("", src));
    }

    #[test]
    fn one_line_if_is_complete() {
        let src = "if a: b = 1\n";
        assert_eq!(split_complete(src), (src, ""));
    }

    #[test]
    fn nested_if_without_closer_stays_open() {
        let src = "if a:\n    if b:\n        c = 1\n    d = 2\n";
        assert_eq!(split_complete(src), ("", src));
    }

    #[test]
    fn nested_if_with_follower_is_complete() {
        let src = "if a:\n    if b:\n        c = 1\n    d = 2\ne = 3\n";
        assert_eq!(split_complete(src), (src, ""));
    }
}
