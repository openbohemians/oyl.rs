//! Safe Rust bindings to [Oyl](https://github.com/openbohemians/oyl), the
//! optimized YAML library: a fast YAML 1.2 parser and emitter written in
//! C11, with SIMD scanning and zero-copy strings.
//!
//! The parser is a pull parser: it hands out events (stream, document,
//! mapping, sequence, scalar, alias) as it reads, so memory stays small
//! however long the input. Strings in events are byte slices into the
//! input, or into the parser's own memory for scalars it had to rewrite
//! (escapes, folding); they stay valid as long as the parser.
//!
//! ```
//! use oyl::{EventKind, Parser};
//!
//! let parser = Parser::new("name: oyl\nlanguages: [C, Rust]\n")?;
//! let mut scalars = Vec::new();
//! for event in parser.events() {
//!     let event = event?;
//!     if event.kind == EventKind::Scalar {
//!         scalars.push(event.value_str()?);
//!     }
//! }
//! assert_eq!(scalars, ["name", "oyl", "languages", "C", "Rust"]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! The [`Emitter`] writes YAML from events, or re-emits a parser's, here
//! with a wider indent (each collection keeps its flow or block style):
//!
//! ```
//! use oyl::{Emitter, Parser};
//!
//! let parser = Parser::new("a:\n  - 1\n  - 2\nb: {c: d}\n")?;
//! let mut emitter = Emitter::new()?;
//! emitter.indent(4);
//! for event in parser.events() {
//!     emitter.event(&event?)?;
//! }
//! assert_eq!(String::from_utf8(emitter.output())?, "a:\n    - 1\n    - 2\nb: {c: d}\n");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! The C library is compiled from sources vendored in the `oyl-sys` crate,
//! so nothing needs to be installed.

use std::cell::Cell;
use std::ffi::{c_char, c_int, CStr};
use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::str::Utf8Error;

use oyl_sys as sys;

// ── Positions, kinds and styles ─────────────────────────────

/// A position in the input: byte offset, and 1-based line and column
/// (the column counts bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mark {
    pub offset: usize,
    pub line: usize,
    pub column: usize,
}

impl From<sys::oyl_mark> for Mark {
    fn from(m: sys::oyl_mark) -> Self {
        Mark { offset: m.offset, line: m.line, column: m.col }
    }
}

/// What an [`Event`] is. Events arrive in a well-formed order:
/// `StreamStart (DocumentStart node DocumentEnd)* StreamEnd`, where a node
/// is a scalar, an alias, or a mapping or sequence holding nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    StreamStart,
    StreamEnd,
    DocumentStart,
    DocumentEnd,
    MappingStart,
    MappingEnd,
    SequenceStart,
    SequenceEnd,
    Scalar,
    Alias,
}

impl EventKind {
    fn from_raw(t: sys::oyl_event_type) -> Option<Self> {
        Some(match t {
            sys::OYL_EVT_STREAM_START => EventKind::StreamStart,
            sys::OYL_EVT_STREAM_END => EventKind::StreamEnd,
            sys::OYL_EVT_DOC_START => EventKind::DocumentStart,
            sys::OYL_EVT_DOC_END => EventKind::DocumentEnd,
            sys::OYL_EVT_MAPPING_START => EventKind::MappingStart,
            sys::OYL_EVT_MAPPING_END => EventKind::MappingEnd,
            sys::OYL_EVT_SEQUENCE_START => EventKind::SequenceStart,
            sys::OYL_EVT_SEQUENCE_END => EventKind::SequenceEnd,
            sys::OYL_EVT_SCALAR => EventKind::Scalar,
            sys::OYL_EVT_ALIAS => EventKind::Alias,
            _ => return None,
        })
    }
}

/// How a scalar was written, or should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ScalarStyle {
    #[default]
    Plain,
    SingleQuoted,
    DoubleQuoted,
    /// A block literal, `|`.
    Literal,
    /// A block folded scalar, `>`.
    Folded,
}

impl ScalarStyle {
    fn from_raw(s: sys::oyl_scalar_style) -> Self {
        match s {
            sys::OYL_SCALAR_SINGLE_QUOTED => ScalarStyle::SingleQuoted,
            sys::OYL_SCALAR_DOUBLE_QUOTED => ScalarStyle::DoubleQuoted,
            sys::OYL_SCALAR_LITERAL => ScalarStyle::Literal,
            sys::OYL_SCALAR_FOLDED => ScalarStyle::Folded,
            _ => ScalarStyle::Plain,
        }
    }

    fn raw(self) -> sys::oyl_scalar_style {
        match self {
            ScalarStyle::Plain => sys::OYL_SCALAR_PLAIN,
            ScalarStyle::SingleQuoted => sys::OYL_SCALAR_SINGLE_QUOTED,
            ScalarStyle::DoubleQuoted => sys::OYL_SCALAR_DOUBLE_QUOTED,
            ScalarStyle::Literal => sys::OYL_SCALAR_LITERAL,
            ScalarStyle::Folded => sys::OYL_SCALAR_FOLDED,
        }
    }
}

// ── Events ──────────────────────────────────────────────────

/// One parsed YAML event. Its strings borrow from the [`Parser`] (and its
/// input), so the event can be kept while parsing goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event<'p> {
    pub kind: EventKind,
    /// A scalar's value or an alias's name; empty for other events.
    pub value: &'p [u8],
    /// The node's anchor (`&name`), if it has one.
    pub anchor: Option<&'p [u8]>,
    /// The node's tag, if it has one or a [`Schema`] resolved one. `!!int`
    /// arrives in full, as `tag:yaml.org,2002:int`.
    pub tag: Option<&'p [u8]>,
    /// For scalars: how it was written.
    pub style: ScalarStyle,
    /// For document starts and ends: no `---` or `...` marker.
    pub implicit: bool,
    /// For mappings and sequences: flow style (`{...}`, `[...]`).
    pub flow: bool,
    pub start: Mark,
    pub end: Mark,
}

impl<'p> Event<'p> {
    /// The value as UTF-8, failing if it isn't (input that is valid UTF-8
    /// gives valid UTF-8 values).
    pub fn value_str(&self) -> Result<&'p str, Utf8Error> {
        std::str::from_utf8(self.value)
    }

    /// The anchor as UTF-8.
    pub fn anchor_str(&self) -> Option<Result<&'p str, Utf8Error>> {
        self.anchor.map(std::str::from_utf8)
    }

    /// The tag as UTF-8.
    pub fn tag_str(&self) -> Option<Result<&'p str, Utf8Error>> {
        self.tag.map(std::str::from_utf8)
    }
}

/// # Safety
/// `s` must point to `len` readable bytes that outlive `'a`.
unsafe fn bytes<'a>(s: sys::oyl_str) -> &'a [u8] {
    if s.data.is_null() || s.len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(s.data.cast(), s.len)
    }
}

/// # Safety
/// As for [`bytes`].
unsafe fn opt_bytes<'a>(s: sys::oyl_str) -> Option<&'a [u8]> {
    if s.data.is_null() { None } else { Some(bytes(s)) }
}

fn raw_str(s: Option<&[u8]>) -> sys::oyl_str {
    match s {
        Some(b) => sys::oyl_str { data: b.as_ptr().cast(), len: b.len() },
        None => sys::oyl_str { data: std::ptr::null(), len: 0 },
    }
}

// ── Errors ──────────────────────────────────────────────────

/// What kind of failure an [`Error`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Allocation failed.
    Memory,
    /// Invalid input to a call.
    Input,
    /// Malformed YAML, found by the scanner.
    Scan,
    /// Malformed YAML, found by the parser.
    Parse,
    /// Events fed to the emitter out of order.
    Emit,
    /// A safety limit was exceeded: the event count, nesting depth, or
    /// alias and merge expansion. See [`Parser::max_events`].
    Limit,
    /// A status this version doesn't know.
    Other,
}

impl ErrorKind {
    fn from_raw(s: sys::oyl_status) -> Self {
        match s {
            sys::OYL_ERR_MEMORY => ErrorKind::Memory,
            sys::OYL_ERR_INPUT => ErrorKind::Input,
            sys::OYL_ERR_SCAN => ErrorKind::Scan,
            sys::OYL_ERR_PARSE => ErrorKind::Parse,
            sys::OYL_ERR_EMIT => ErrorKind::Emit,
            sys::OYL_ERR_LIMIT => ErrorKind::Limit,
            _ => ErrorKind::Other,
        }
    }
}

/// A parse or emit failure, with Oyl's message and, for parse errors,
/// where in the input it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    mark: Option<Mark>,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Where in the input the error is, for parse errors.
    pub fn mark(&self) -> Option<Mark> {
        self.mark
    }

    fn from_status(status: sys::oyl_status) -> Self {
        Error { kind: ErrorKind::from_raw(status), message: status_name(status), mark: None }
    }

    fn memory() -> Self {
        Error::from_status(sys::OYL_ERR_MEMORY)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.mark {
            Some(m) => write!(f, "{} at line {}, column {}", self.message, m.line, m.column),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for Error {}

fn status_name(status: sys::oyl_status) -> String {
    // SAFETY: oyl_status_str returns a static C string, or NULL
    unsafe { c_string(sys::oyl_status_str(status)) }.unwrap_or_else(|| format!("status {status}"))
}

/// # Safety
/// `p` is NULL or a valid C string.
unsafe fn c_string(p: *const c_char) -> Option<String> {
    if p.is_null() { None } else { Some(CStr::from_ptr(p).to_string_lossy().into_owned()) }
}

fn check(status: sys::oyl_status) -> Result<(), Error> {
    if status == sys::OYL_OK { Ok(()) } else { Err(Error::from_status(status)) }
}

fn clamp_int(n: u32) -> c_int {
    c_int::try_from(n).unwrap_or(c_int::MAX)
}

// ── Schemas ─────────────────────────────────────────────────

/// A tag schema: given to a parser, it resolves the tags of untagged
/// nodes (`42` becomes `tag:yaml.org,2002:int` under the core schema).
#[derive(Clone, Copy)]
pub struct Schema(NonNull<sys::oyl_schema>);

// SAFETY: the presets are static and never change
unsafe impl Send for Schema {}
unsafe impl Sync for Schema {}

impl Schema {
    /// YAML 1.2 failsafe: everything is `!!str`, `!!seq` or `!!map`.
    pub fn failsafe() -> Self {
        Schema::preset(unsafe { sys::oyl_schema_failsafe() })
    }

    /// YAML 1.2 JSON: null, true/false, integers and floats as JSON writes them.
    pub fn json() -> Self {
        Schema::preset(unsafe { sys::oyl_schema_json() })
    }

    /// YAML 1.2 core: JSON plus `Null`/`NULL`/`~`, `True`/`TRUE`, `0x`/`0o`
    /// integers, `.inf`, `.nan`, and so on.
    pub fn core() -> Self {
        Schema::preset(unsafe { sys::oyl_schema_core() })
    }

    fn preset(p: *const sys::oyl_schema) -> Self {
        Schema(NonNull::new(p.cast_mut()).expect("Oyl's preset schemas are static"))
    }

    /// The tag this schema gives a scalar with this value and style:
    /// quoted scalars are strings, plain ones are matched against the rules.
    pub fn resolve(&self, value: &[u8], style: ScalarStyle) -> &'static [u8] {
        // SAFETY: a preset's tags are static strings
        unsafe { bytes(sys::oyl_schema_resolve(self.0.as_ptr(), raw_str(Some(value)), style.raw())) }
    }
}

impl fmt::Debug for Schema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Schema")
    }
}

// ── Parser ──────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Parsing,
    Done,
    Failed,
}

/// A YAML pull parser over a borrowed input.
///
/// Set options (merge keys, alias resolution, a schema, limits) before
/// reading the first event. After the stream ends, or after an error, it
/// yields no more events.
pub struct Parser<'i> {
    arena: NonNull<sys::oyl_arena>,
    parser: NonNull<sys::oyl_parser>,
    state: Cell<State>,
    _input: PhantomData<&'i [u8]>,
}

// SAFETY: the parser and its arena belong to this value alone and the
// input is shared immutably; next_event takes &self, so it isn't Sync.
unsafe impl Send for Parser<'_> {}

impl<'i> Parser<'i> {
    /// A parser over `input` (a `str`, `[u8]`, `String` or `Vec<u8>`).
    pub fn new<T: AsRef<[u8]> + ?Sized>(input: &'i T) -> Result<Self, Error> {
        let input = input.as_ref();
        let arena = NonNull::new(unsafe { sys::oyl_arena_new(input.len().clamp(4096, 1 << 20)) })
            .ok_or_else(Error::memory)?;
        let parser = unsafe { sys::oyl_parser_new(input.as_ptr().cast(), input.len(), arena.as_ptr()) };
        match NonNull::new(parser) {
            Some(parser) => Ok(Parser { arena, parser, state: Cell::new(State::Parsing), _input: PhantomData }),
            None => {
                unsafe { sys::oyl_arena_free(arena.as_ptr()) };
                Err(Error::memory())
            }
        }
    }

    /// Expand merge keys (`<<: *base`). Off by default. A merge value must
    /// be a mapping, an alias to one, or a sequence of those.
    pub fn merge(&mut self, enable: bool) -> &mut Self {
        unsafe { sys::oyl_parser_set_merge(self.parser.as_ptr(), enable) };
        self
    }

    /// Replace aliases with copies of their anchored nodes. Off by default.
    /// Cyclic aliases, and aliases with no anchor before them, stay aliases.
    pub fn resolve(&mut self, enable: bool) -> &mut Self {
        unsafe { sys::oyl_parser_set_resolve(self.parser.as_ptr(), enable) };
        self
    }

    /// Stop with an [`ErrorKind::Limit`] error after this many events
    /// (alias and merge expansion included). Default 10,000; 0 turns the
    /// limit off.
    pub fn max_events(&mut self, max: u32) -> &mut Self {
        unsafe { sys::oyl_parser_set_max_events(self.parser.as_ptr(), clamp_int(max)) };
        self
    }

    /// Stop with an [`ErrorKind::Limit`] error past this nesting depth.
    /// Default 256; 0 turns the limit off, but very deep input can then
    /// overflow the stack in some modes.
    pub fn max_depth(&mut self, max: u32) -> &mut Self {
        unsafe { sys::oyl_parser_set_max_depth(self.parser.as_ptr(), clamp_int(max)) };
        self
    }

    /// Resolve the tags of untagged nodes with this schema.
    pub fn schema(&mut self, schema: Schema) -> &mut Self {
        unsafe { sys::oyl_parser_set_schema(self.parser.as_ptr(), schema.0.as_ptr()) };
        self
    }

    /// The next event; `Ok(None)` once the stream has ended.
    pub fn next_event(&self) -> Result<Option<Event<'_>>, Error> {
        if self.state.get() != State::Parsing {
            return Ok(None);
        }
        let mut raw: *const sys::oyl_event = std::ptr::null();
        let status = unsafe { sys::oyl_parse_next(self.parser.as_ptr(), &mut raw) };
        if status != sys::OYL_OK || raw.is_null() {
            self.state.set(State::Failed);
            return Err(self.error(status));
        }
        // SAFETY: on success Oyl hands out a valid event, which we copy
        // before the next call; its strings point into the input or the
        // arena, which outlive the borrow of self.
        let ev = unsafe { &*raw };
        if ev.type_ == sys::OYL_EVT_NONE {
            self.state.set(State::Done);
            return Ok(None);
        }
        let Some(kind) = EventKind::from_raw(ev.type_) else {
            self.state.set(State::Failed);
            return Err(Error { kind: ErrorKind::Other, message: format!("unknown event type {}", ev.type_), mark: None });
        };
        if kind == EventKind::StreamEnd {
            self.state.set(State::Done);
        }
        unsafe {
            Ok(Some(Event {
                kind,
                value: bytes(ev.value),
                anchor: opt_bytes(ev.anchor),
                tag: opt_bytes(ev.tag),
                style: ScalarStyle::from_raw(ev.scalar_style),
                implicit: ev.implicit,
                flow: ev.flow,
                start: ev.start.into(),
                end: ev.end.into(),
            }))
        }
    }

    /// The events, as an iterator of `Result<Event, Error>`; it ends after
    /// the stream does, or after an error.
    pub fn events(&self) -> Events<'_, 'i> {
        Events { parser: self }
    }

    fn error(&self, status: sys::oyl_status) -> Error {
        let p = self.parser.as_ptr();
        // SAFETY: Oyl's error message is NULL or a C string owned by the parser
        let message = unsafe { c_string(sys::oyl_parser_error(p)) }.unwrap_or_else(|| status_name(status));
        let mark = Mark::from(unsafe { sys::oyl_parser_error_mark(p) });
        let kind = if status == sys::OYL_OK { ErrorKind::Other } else { ErrorKind::from_raw(status) };
        Error { kind, message, mark: (mark.line > 0).then_some(mark) }
    }
}

impl Drop for Parser<'_> {
    fn drop(&mut self) {
        unsafe {
            sys::oyl_parser_free(self.parser.as_ptr());
            sys::oyl_arena_free(self.arena.as_ptr());
        }
    }
}

impl fmt::Debug for Parser<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Parser").finish_non_exhaustive()
    }
}

/// The iterator [`Parser::events`] returns.
#[derive(Debug)]
pub struct Events<'p, 'i> {
    parser: &'p Parser<'i>,
}

impl<'p, 'i> Iterator for Events<'p, 'i> {
    type Item = Result<Event<'p>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.parser.next_event().transpose()
    }
}

impl std::iter::FusedIterator for Events<'_, '_> {}

// ── Emitter ─────────────────────────────────────────────────

/// The emitter's output style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EmitStyle {
    /// Indented block style.
    #[default]
    Block,
    /// Flow style: `{...}` and `[...]`.
    Flow,
    /// Minimal whitespace.
    Minimal,
}

/// Writes YAML from events, fed in the order a parser produces them:
/// `stream_start`, then documents, then `stream_end`.
///
/// Its output always parses back to the same data: scalars are quoted when
/// plain style would read back differently, and so on.
pub struct Emitter {
    arena: NonNull<sys::oyl_arena>,
    emitter: NonNull<sys::oyl_emitter>,
}

// SAFETY: the emitter and its arena belong to this value alone
unsafe impl Send for Emitter {}

impl Emitter {
    /// An emitter writing block style with a 2-space indent.
    pub fn new() -> Result<Self, Error> {
        let arena = NonNull::new(unsafe { sys::oyl_arena_new(4096) }).ok_or_else(Error::memory)?;
        match NonNull::new(unsafe { sys::oyl_emitter_new(arena.as_ptr()) }) {
            Some(emitter) => Ok(Emitter { arena, emitter }),
            None => {
                unsafe { sys::oyl_arena_free(arena.as_ptr()) };
                Err(Error::memory())
            }
        }
    }

    pub fn style(&mut self, style: EmitStyle) -> &mut Self {
        let raw = match style {
            EmitStyle::Block => sys::OYL_EMIT_BLOCK,
            EmitStyle::Flow => sys::OYL_EMIT_FLOW,
            EmitStyle::Minimal => sys::OYL_EMIT_MINIMAL,
        };
        unsafe { sys::oyl_emitter_set_style(self.e(), raw) };
        self
    }

    /// Spaces per indentation level, 1 to 10 (default 2).
    pub fn indent(&mut self, spaces: u8) -> &mut Self {
        unsafe { sys::oyl_emitter_set_indent(self.e(), c_int::from(spaces)) };
        self
    }

    pub fn stream_start(&mut self) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_stream_start(self.e()) })
    }

    pub fn stream_end(&mut self) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_stream_end(self.e()) })
    }

    /// Start a document; `implicit` leaves out the `---` marker.
    pub fn document_start(&mut self, implicit: bool) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_document_start(self.e(), implicit) })
    }

    /// End a document; `implicit` leaves out the `...` marker.
    pub fn document_end(&mut self, implicit: bool) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_document_end(self.e(), implicit) })
    }

    /// A scalar. [`ScalarStyle::Plain`] writes it plain when that reads
    /// back as the same text, and quotes it otherwise. Tags are full
    /// (`tag:yaml.org,2002:str`, written `!!str`) or local (`!foo`).
    pub fn scalar(&mut self, value: impl AsRef<[u8]>, style: ScalarStyle, anchor: Option<&str>, tag: Option<&str>) -> Result<(), Error> {
        self.raw_scalar(value.as_ref(), style, anchor.map(str::as_bytes), tag.map(str::as_bytes))
    }

    /// An alias (`*name`).
    pub fn alias(&mut self, name: &str) -> Result<(), Error> {
        self.raw_alias(name.as_bytes())
    }

    /// Start a mapping; `flow` writes it in flow style (`{...}`).
    pub fn mapping_start(&mut self, anchor: Option<&str>, tag: Option<&str>, flow: bool) -> Result<(), Error> {
        self.raw_mapping_start(anchor.map(str::as_bytes), tag.map(str::as_bytes), flow)
    }

    pub fn mapping_end(&mut self) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_mapping_end(self.e()) })
    }

    /// Start a sequence; `flow` writes it in flow style (`[...]`).
    pub fn sequence_start(&mut self, anchor: Option<&str>, tag: Option<&str>, flow: bool) -> Result<(), Error> {
        self.raw_sequence_start(anchor.map(str::as_bytes), tag.map(str::as_bytes), flow)
    }

    pub fn sequence_end(&mut self) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_sequence_end(self.e()) })
    }

    /// Emit a parsed event, e.g. to reformat a document.
    pub fn event(&mut self, event: &Event<'_>) -> Result<(), Error> {
        match event.kind {
            EventKind::StreamStart => self.stream_start(),
            EventKind::StreamEnd => self.stream_end(),
            EventKind::DocumentStart => self.document_start(event.implicit),
            EventKind::DocumentEnd => self.document_end(event.implicit),
            EventKind::MappingStart => self.raw_mapping_start(event.anchor, event.tag, event.flow),
            EventKind::MappingEnd => self.mapping_end(),
            EventKind::SequenceStart => self.raw_sequence_start(event.anchor, event.tag, event.flow),
            EventKind::SequenceEnd => self.sequence_end(),
            EventKind::Scalar => self.raw_scalar(event.value, event.style, event.anchor, event.tag),
            EventKind::Alias => self.raw_alias(event.value),
        }
    }

    /// The YAML written so far.
    pub fn output(&self) -> Vec<u8> {
        // SAFETY: Oyl copies the output into the arena, which outlives this call
        unsafe { bytes(sys::oyl_emitter_output(self.e())) }.to_vec()
    }

    // The emitter writes each string out before returning, keeping no
    // pointer to it, so borrowed strings are fine.

    fn raw_scalar(&mut self, value: &[u8], style: ScalarStyle, anchor: Option<&[u8]>, tag: Option<&[u8]>) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_scalar(self.e(), raw_str(Some(value)), style.raw(), raw_str(anchor), raw_str(tag)) })
    }

    fn raw_alias(&mut self, name: &[u8]) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_alias(self.e(), raw_str(Some(name))) })
    }

    fn raw_mapping_start(&mut self, anchor: Option<&[u8]>, tag: Option<&[u8]>, flow: bool) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_mapping_start(self.e(), raw_str(anchor), raw_str(tag), flow) })
    }

    fn raw_sequence_start(&mut self, anchor: Option<&[u8]>, tag: Option<&[u8]>, flow: bool) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_sequence_start(self.e(), raw_str(anchor), raw_str(tag), flow) })
    }

    fn e(&self) -> *mut sys::oyl_emitter {
        self.emitter.as_ptr()
    }
}

impl Drop for Emitter {
    fn drop(&mut self) {
        unsafe {
            sys::oyl_emitter_free(self.emitter.as_ptr());
            sys::oyl_arena_free(self.arena.as_ptr());
        }
    }
}

impl fmt::Debug for Emitter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Emitter").finish_non_exhaustive()
    }
}
