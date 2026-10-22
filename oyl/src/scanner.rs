// The scanner: YAML's tokens (oyl_scanner), below the parser.

use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::str::Utf8Error;

use oyl_sys as sys;

use crate::{bytes, c_string, status_name, Error, ErrorKind, Mark, ScalarStyle, State};

/// What a [`Token`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TokenKind {
    /// The start of the input.
    StreamStart,
    /// The end of the input; the last token.
    StreamEnd,
    /// `---`.
    DocumentStart,
    /// `...`.
    DocumentEnd,
    /// `-`, before a block sequence's item.
    BlockSequenceEntry,
    /// `?`, before an explicit key.
    MappingKey,
    /// `:`, before a value.
    MappingValue,
    /// `[`.
    FlowSequenceStart,
    /// `]`.
    FlowSequenceEnd,
    /// `{`.
    FlowMappingStart,
    /// `}`.
    FlowMappingEnd,
    /// `,`.
    FlowEntry,
    /// A scalar in any style: [`Token::value`] is its text.
    Scalar,
    /// A tag as written (`!!str`, `!e!x`, `!<tag:a,b:c>`).
    Tag,
    /// An anchor (`&name`): the value is the name.
    Anchor,
    /// An alias (`*name`): the value is the name.
    Alias,
    /// A directive: the value is the whole line (`%YAML 1.2`).
    Directive,
}

impl TokenKind {
    fn from_raw(t: sys::oyl_token_type) -> Option<Self> {
        Some(match t {
            sys::OYL_TOK_STREAM_START => TokenKind::StreamStart,
            sys::OYL_TOK_STREAM_END => TokenKind::StreamEnd,
            sys::OYL_TOK_DOC_START => TokenKind::DocumentStart,
            sys::OYL_TOK_DOC_END => TokenKind::DocumentEnd,
            sys::OYL_TOK_BLOCK_SEQ_ENTRY => TokenKind::BlockSequenceEntry,
            sys::OYL_TOK_BLOCK_MAP_KEY => TokenKind::MappingKey,
            sys::OYL_TOK_BLOCK_MAP_VALUE => TokenKind::MappingValue,
            sys::OYL_TOK_FLOW_SEQ_START => TokenKind::FlowSequenceStart,
            sys::OYL_TOK_FLOW_SEQ_END => TokenKind::FlowSequenceEnd,
            sys::OYL_TOK_FLOW_MAP_START => TokenKind::FlowMappingStart,
            sys::OYL_TOK_FLOW_MAP_END => TokenKind::FlowMappingEnd,
            sys::OYL_TOK_FLOW_ENTRY => TokenKind::FlowEntry,
            sys::OYL_TOK_SCALAR => TokenKind::Scalar,
            sys::OYL_TOK_TAG => TokenKind::Tag,
            sys::OYL_TOK_ANCHOR => TokenKind::Anchor,
            sys::OYL_TOK_ALIAS => TokenKind::Alias,
            sys::OYL_TOK_DIRECTIVE => TokenKind::Directive,
            _ => return None,
        })
    }
}

/// One token. Its value borrows from the [`Scanner`] (and its input), so
/// the token can be kept while scanning goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token<'s> {
    /// What the token is.
    pub kind: TokenKind,
    /// A scalar's text (escapes and folding applied), a tag as written, an
    /// anchor's or alias's name, or a directive's line; empty for others.
    pub value: &'s [u8],
    /// For scalars: how it was written.
    pub style: ScalarStyle,
    /// Where it starts in the input.
    pub start: Mark,
    /// Where it ends.
    pub end: Mark,
}

impl<'s> Token<'s> {
    /// The value as UTF-8.
    pub fn value_str(&self) -> Result<&'s str, Utf8Error> {
        std::str::from_utf8(self.value)
    }
}

/// YAML's tokens over a borrowed input: indicators, scalars, tags, anchors
/// and directives, without the structure the [`Parser`](crate::Parser)
/// finds in them (an implicit key gets no [`TokenKind::MappingKey`], and an
/// unclosed `[` is no error here). For syntax highlighting and the like;
/// most programs want the parser.
///
/// ```
/// use oyl::{Scanner, TokenKind};
///
/// let scanner = Scanner::new("a: [b, 'c']\n")?;
/// let kinds: Vec<_> = scanner.tokens().map(|t| t.map(|t| t.kind)).collect::<Result<_, _>>()?;
/// assert_eq!(kinds, [
///     TokenKind::StreamStart, TokenKind::Scalar, TokenKind::MappingValue,
///     TokenKind::FlowSequenceStart, TokenKind::Scalar, TokenKind::FlowEntry,
///     TokenKind::Scalar, TokenKind::FlowSequenceEnd, TokenKind::StreamEnd,
/// ]);
/// # Ok::<(), oyl::Error>(())
/// ```
///
/// After the stream ends, or after an error, it yields no more tokens.
pub struct Scanner<'i> {
    arena: NonNull<sys::oyl_arena>,
    scanner: NonNull<sys::oyl_scanner>,
    state: Cell<State>,
    _input: PhantomData<&'i [u8]>,
}

// SAFETY: the scanner and its arena belong to this value alone and the
// input is shared immutably; next_token takes &self, so it isn't Sync.
unsafe impl Send for Scanner<'_> {}

impl<'i> Scanner<'i> {
    /// A scanner over `input` (a `str`, `[u8]`, `String` or `Vec<u8>`).
    pub fn new<T: AsRef<[u8]> + ?Sized>(input: &'i T) -> Result<Self, Error> {
        let input = input.as_ref();
        let arena = NonNull::new(unsafe { sys::oyl_arena_new(input.len().clamp(4096, 1 << 20)) })
            .ok_or_else(Error::memory)?;
        let scanner = unsafe { sys::oyl_scanner_new(input.as_ptr().cast(), input.len(), arena.as_ptr()) };
        match NonNull::new(scanner) {
            Some(scanner) => Ok(Scanner { arena, scanner, state: Cell::new(State::Parsing), _input: PhantomData }),
            None => {
                unsafe { sys::oyl_arena_free(arena.as_ptr()) };
                Err(Error::memory())
            }
        }
    }

    /// The next token; `Ok(None)` once the stream has ended.
    pub fn next_token(&self) -> Result<Option<Token<'_>>, Error> {
        if self.state.get() != State::Parsing {
            return Ok(None);
        }
        let mut raw: *const sys::oyl_token = std::ptr::null();
        let status = unsafe { sys::oyl_scan_next(self.scanner.as_ptr(), &mut raw) };
        if status != sys::OYL_OK || raw.is_null() {
            self.state.set(State::Failed);
            return Err(self.error(status));
        }
        // SAFETY: on success Oyl hands out a valid token, which we copy
        // before the next call; its value points into the input or the
        // arena, which outlive the borrow of self
        let tok = unsafe { &*raw };
        if tok.type_ == sys::OYL_TOK_NONE {
            self.state.set(State::Done);
            return Ok(None);
        }
        let Some(kind) = TokenKind::from_raw(tok.type_) else {
            self.state.set(State::Failed);
            return Err(Error { kind: ErrorKind::Other, message: format!("unknown token type {}", tok.type_), mark: None });
        };
        if kind == TokenKind::StreamEnd {
            self.state.set(State::Done);
        }
        Ok(Some(Token {
            kind,
            value: unsafe { bytes(tok.value) },
            style: ScalarStyle::from_raw(tok.scalar_style),
            start: tok.start.into(),
            end: tok.end.into(),
        }))
    }

    /// The tokens, as an iterator of `Result<Token, Error>`; it ends after
    /// the stream does, or after an error.
    pub fn tokens(&self) -> Tokens<'_, 'i> {
        Tokens { scanner: self }
    }

    fn error(&self, status: sys::oyl_status) -> Error {
        let s = self.scanner.as_ptr();
        // SAFETY: Oyl's error message is NULL or a C string owned by the scanner
        let message = unsafe { c_string(sys::oyl_scanner_error(s)) }.unwrap_or_else(|| status_name(status));
        let mark = Mark::from(unsafe { sys::oyl_scanner_error_mark(s) });
        let kind = if status == sys::OYL_OK { ErrorKind::Other } else { ErrorKind::from_raw(status) };
        Error { kind, message, mark: (mark.line > 0).then_some(mark) }
    }
}

impl Drop for Scanner<'_> {
    fn drop(&mut self) {
        unsafe {
            sys::oyl_scanner_free(self.scanner.as_ptr());
            sys::oyl_arena_free(self.arena.as_ptr());
        }
    }
}

impl fmt::Debug for Scanner<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scanner").finish_non_exhaustive()
    }
}

/// The iterator [`Scanner::tokens`] returns.
#[derive(Debug)]
pub struct Tokens<'s, 'i> {
    scanner: &'s Scanner<'i>,
}

impl<'s> Iterator for Tokens<'s, '_> {
    type Item = Result<Token<'s>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.scanner.next_token().transpose()
    }
}

impl std::iter::FusedIterator for Tokens<'_, '_> {}
