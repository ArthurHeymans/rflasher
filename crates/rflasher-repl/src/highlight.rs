//! Syntax highlighting for the Steel REPL
//!
//! Provides syntax highlighting, bracket matching, and input validation.

use colored::Colorize;
use crossbeam_utils::atomic::AtomicCell;
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::{ValidationContext, ValidationResult, Validator};
use rustyline::{Context, Helper};
use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use steel_parser::interner::InternedString;
use steel_parser::lexer::TokenStream;
use steel_parser::parser::SourceId;
use steel_parser::span::Span;
use steel_parser::tokens::TokenType;

/// Helper struct for rustyline that provides syntax highlighting,
/// bracket matching, completion, and input validation.
#[derive(Helper)]
pub struct ReplHelper {
    /// Global identifiers for completion and highlighting
    globals: Arc<Mutex<HashSet<InternedString>>>,
    /// Current bracket position for matching
    bracket: AtomicCell<Option<(u8, usize)>>,
}

impl ReplHelper {
    /// Create a new ReplHelper with the given set of global identifiers
    pub fn new(globals: Arc<Mutex<HashSet<InternedString>>>) -> Self {
        Self {
            globals,
            bracket: AtomicCell::new(None),
        }
    }
}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        // Find the current identifier being typed
        let mut found: Option<(Span, Cow<'_, str>)> = None;

        for token in TokenStream::new(line, true, SourceId::none()) {
            if let TokenType::Identifier(ref symbol) = token.ty {
                let span = token.span();
                if (span.start()..=span.end()).contains(&pos) {
                    found = Some((span, symbol.clone()));
                    break;
                }
            }
        }

        let Some((span, symbol)) = found else {
            return Ok((0, Vec::new()));
        };

        let symbol_str = symbol.as_ref();

        // Find all globals containing the identifier
        let mut starting = Vec::new();
        let mut containing = Vec::new();
        for interned in self.globals.lock().unwrap().iter() {
            let s = interned.resolve();
            if s.starts_with(symbol_str) {
                starting.push(s.to_owned());
            } else if s.contains(symbol_str) {
                containing.push(s.to_owned());
            }
        }

        // Sort and prioritize completions
        let compare = |a: &String, b: &String| {
            a.contains("builtin")
                .cmp(&b.contains("builtin"))
                .then(a.starts_with('#').cmp(&b.starts_with('#')))
                .then(a.cmp(b))
        };
        starting.sort_by(compare);
        containing.sort_by(compare);

        let completions = starting
            .into_iter()
            .chain(containing)
            .map(|ident| Pair {
                display: format!("{}", ident.white()),
                replacement: ident,
            })
            .collect();

        Ok((span.start(), completions))
    }

    fn update(
        &self,
        line: &mut rustyline::line_buffer::LineBuffer,
        start: usize,
        elected: &str,
        cl: &mut rustyline::Changeset,
    ) {
        for token in TokenStream::new(line, true, SourceId::none()) {
            if token.span().start() == start {
                line.replace(start..token.span().end(), elected, cl);
                break;
            }
        }
    }
}

impl Validator for ReplHelper {
    fn validate(&self, ctx: &mut ValidationContext) -> rustyline::Result<ValidationResult> {
        let token_stream = TokenStream::new(ctx.input(), true, SourceId::none());
        let mut balance = 0;
        let mut has_error = false;

        for token in token_stream {
            match &token.ty {
                TokenType::OpenParen(..) => balance += 1,
                TokenType::CloseParen(_) => balance -= 1,
                TokenType::Error => {
                    // May be incomplete input
                    has_error = true;
                }
                _ => {}
            }
        }

        // If we have unbalanced parens or an error (likely incomplete string/comment),
        // consider input incomplete
        if balance > 0 || (has_error && balance >= 0) {
            Ok(ValidationResult::Incomplete)
        } else {
            Ok(ValidationResult::Valid(None))
        }
    }
}

impl Hinter for ReplHelper {
    type Hint = String;

    fn hint(&self, _line: &str, _pos: usize, _context: &Context) -> Option<String> {
        None
    }
}

impl Highlighter for ReplHelper {
    fn highlight<'l>(&self, line: &'l str, pos: usize) -> Cow<'l, str> {
        let mut highlighted = line.to_owned();
        let mut token_stream = TokenStream::new(line, true, SourceId::none()).peekable();

        let mut ranges_to_replace: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        let mut stack = vec![];
        let mut cursor: Option<(_, Span)> = None;
        let mut paren_to_highlight: Option<usize> = None;

        while let Some(token) = token_stream.next() {
            match &token.ty {
                TokenType::OpenParen(paren, paren_mod) if paren_to_highlight.is_none() => {
                    let open_span = TokenType::open_span(token.span, *paren_mod);

                    if open_span.start() == pos
                        || (open_span.start() == pos + 1 && cursor.is_none())
                    {
                        cursor = Some((*paren, open_span));
                    }

                    stack.push((*paren, open_span));
                }

                TokenType::CloseParen(paren) if paren_to_highlight.is_none() => {
                    let mut matches = token.span.start() == pos;

                    if token.span.end() == pos {
                        let next_span = match token_stream.peek() {
                            Some(steel_parser::tokens::Token {
                                ty: TokenType::CloseParen(_),
                                span,
                                ..
                            }) => Some(*span),

                            Some(steel_parser::tokens::Token {
                                ty: TokenType::OpenParen(_, paren_mod),
                                span,
                                ..
                            }) => Some(TokenType::open_span(*span, *paren_mod)),

                            _ => None,
                        };

                        matches = match next_span {
                            Some(span) => span.start() > pos,
                            _ => true,
                        }
                    }

                    if matches {
                        cursor = Some((*paren, token.span));
                    }

                    match (stack.pop(), cursor) {
                        (Some((open, span)), Some((_, cursor_span))) if open == *paren => {
                            if cursor_span == span {
                                paren_to_highlight = Some(token.span.start());
                            } else if cursor_span == token.span {
                                paren_to_highlight = Some(span.start());
                            }
                        }
                        _ => {}
                    }
                }

                // Keywords - purple
                TokenType::Lambda
                | TokenType::If
                | TokenType::Define
                | TokenType::Let
                | TokenType::Require => {
                    let colored = format!("{}", token.source().bright_purple());
                    ranges_to_replace.push((token.span().range(), colored));
                }

                // Booleans - magenta
                TokenType::BooleanLiteral(_) => {
                    let colored = format!("{}", token.source().bright_magenta());
                    ranges_to_replace.push((token.span().range(), colored));
                }

                // Known identifiers - blue
                TokenType::Identifier(ident) => {
                    // For Cow<str>, convert to owned string and check
                    let ident_str = ident.as_ref();
                    let is_known = self
                        .globals
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|g| g.resolve() == ident_str);
                    if is_known {
                        let colored = format!("{}", token.source().bright_blue());
                        ranges_to_replace.push((token.span().range(), colored));
                    }
                }

                // Numbers - yellow
                TokenType::Number(_) => {
                    let colored = format!("{}", token.source().bright_yellow());
                    ranges_to_replace.push((token.span().range(), colored));
                }

                // Strings - green
                TokenType::StringLiteral(_) => {
                    let colored = format!("{}", token.source().bright_green());
                    ranges_to_replace.push((token.span().range(), colored));
                }

                _ => {}
            }
        }

        // Apply replacements in reverse order to preserve positions
        let mut offset = 0;
        for (range, replacement) in ranges_to_replace.into_iter().rev() {
            let old_length = highlighted.len();
            let start = range.start;
            highlighted.replace_range(range, &replacement);
            let new_length = highlighted.len();

            if let Some(paren_pos) = paren_to_highlight {
                if start <= paren_pos {
                    offset += new_length - old_length;
                }
            }
        }

        // Highlight matching parenthesis
        if let Some(paren_pos) = paren_to_highlight {
            let idx = if paren_pos == 0 {
                0
            } else {
                paren_pos + offset
            };

            if idx < highlighted.len() {
                highlighted.replace_range(
                    idx..=idx,
                    &format!("\x1b[1;34m{}\x1b[0m", highlighted.as_bytes()[idx] as char),
                );
            }
        }

        Cow::Owned(highlighted)
    }

    fn highlight_char(&self, line: &str, mut pos: usize, _forced: bool) -> bool {
        self.bracket.store(check_bracket(line, pos));
        if self.bracket.load().is_some() {
            return true;
        }

        if line.is_empty() {
            return false;
        }

        if pos >= line.len() {
            pos = line.len() - 1;
            let b = line.as_bytes()[pos];
            match b {
                b'"' | b' ' => true,
                x if x.is_ascii_digit() => true,
                _ => false,
            }
        } else {
            self.bracket.load().is_some()
        }
    }
}

/// Check for bracket under or before cursor
fn check_bracket(line: &str, pos: usize) -> Option<(u8, usize)> {
    if line.is_empty() {
        return None;
    }

    let bytes = line.as_bytes();

    let on_bracket = |pos: usize| {
        let b = bytes.get(pos).copied()?;
        let open = is_open_bracket(b);
        let close = is_close_bracket(b);

        if (open && (pos + 1 < bytes.len())) || (close && pos > 0) {
            Some((b, open))
        } else {
            None
        }
    };

    if let Some((current, _)) = on_bracket(pos) {
        return Some((current, pos));
    }

    if pos > 0 {
        if let Some((current, open)) = on_bracket(pos - 1) {
            if !open {
                return Some((current, pos - 1));
            }
        }
    }

    if let Some((current, open)) = on_bracket(pos + 1) {
        if open {
            return Some((current, pos + 1));
        }
    }

    None
}

fn is_open_bracket(bracket: u8) -> bool {
    matches!(bracket, b'{' | b'[' | b'(')
}

fn is_close_bracket(bracket: u8) -> bool {
    matches!(bracket, b'}' | b']' | b')')
}
