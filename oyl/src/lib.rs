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
//! with a wider indent (each collection keeps its flow or block style, and
//! each document its `%YAML` and `%TAG` directives):
//!
//! ```
//! use oyl::{Emitter, Parser};
//!
//! let parser = Parser::new("a:\n  - 1\n  - 2\nb: {c: d}\n")?;
//! let mut emitter = Emitter::new()?;
//! emitter.indent(4);
//! emitter.reemit(&parser)?;
//! assert_eq!(String::from_utf8(emitter.output())?, "a:\n    - 1\n    - 2\nb: {c: d}\n");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! A [`Tree`] holds a document in memory, for lookups and typed values
//! without handling events: `get` follows aliases and applies `<<` merges,
//! and each scalar's value is typed by the document's [`Dialect`]:
//!
//! ```
//! use oyl::{Parser, Value};
//!
//! let parser = Parser::new("base: &b {replicas: 2}\nweb:\n  <<: *b\n  image: nginx\n")?;
//! let tree = parser.next_tree()?.expect("a document");
//! let web = tree.root().get("web").expect("a key");
//! assert_eq!(web.get("image").unwrap().text(), b"nginx");
//! assert_eq!(web.get("replicas").unwrap().value()?, Value::Int(2));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! A program can build its own schemas ([`SchemaBuilder`]), with types of
//! its own, and dialects ([`CustomDialect`]).
//!
//! With the `serde` feature, `oyl::from_str` and friends
//! deserialize YAML into your types, from its tree (see the `de` module),
//! and `oyl::to_string` and friends write your types as YAML (the `ser`
//! module).
//!
//! The C library is compiled from sources vendored in the `oyl-sys` crate,
//! so nothing needs to be installed.

#![warn(missing_docs)]
// docs.rs (nightly, --cfg docsrs) labels what a feature enables
#![cfg_attr(docsrs, feature(doc_cfg))]

use std::any::Any;
use std::cell::Cell;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::str::Utf8Error;
use std::sync::atomic::{AtomicBool, Ordering};

/// The raw C declarations (the `oyl-sys` crate): the types and constants
/// of [`Tree::raw_nodes`], [`Tree::raw_props`] and [`Tree::raw_pool`].
pub use oyl_sys as sys;

mod custom;
mod scanner;
mod tree;
pub use custom::{CustomDialect, CustomSchema, FloatFlags, IntFlags, SchemaBuilder, Syntax};
pub use scanner::{Scanner, Token, TokenKind, Tokens};
pub use tree::{Items, Node, NodeKind, Pairs, Tree, Trees};

#[cfg(feature = "serde")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
pub mod de;
#[cfg(feature = "serde")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
pub use de::{from_node, from_reader, from_slice, from_slice_with, from_str, from_str_with};
#[cfg(feature = "serde")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
pub mod ser;
#[cfg(feature = "serde")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
pub use ser::{to_string, to_vec, to_writer, Serializer};

// ── Positions, kinds and styles ─────────────────────────────

/// A position in the input: byte offset, and 1-based line and column
/// (the column counts bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mark {
    /// Bytes from the start of the input.
    pub offset: usize,
    /// The line, from 1.
    pub line: usize,
    /// The column, from 1, in bytes.
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
    /// The start of the input.
    StreamStart,
    /// The end of the input; the last event.
    StreamEnd,
    /// A document begins, with `---` or without ([`Event::implicit`]).
    DocumentStart,
    /// A document ends, with `...` or without.
    DocumentEnd,
    /// A mapping begins; its keys and values follow, alternating.
    MappingStart,
    /// The mapping ends.
    MappingEnd,
    /// A sequence begins; its items follow.
    SequenceStart,
    /// The sequence ends.
    SequenceEnd,
    /// A scalar: its text is [`Event::value`].
    Scalar,
    /// An alias (`*name`): the name is [`Event::value`].
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
    /// Unquoted: `text`.
    #[default]
    Plain,
    /// In single quotes: `'text'`.
    SingleQuoted,
    /// In double quotes, with escapes: `"text\n"`.
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
    /// What the event is.
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
    /// Where its text starts in the input.
    pub start: Mark,
    /// Where its text ends.
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

// ── Directives ──────────────────────────────────────────────

/// A document's directives: its `%YAML` version and its `%TAG` handles.
/// They aren't among its events: [`Parser::directives`] and
/// [`Tree::directives`] give them, and [`Emitter::directives`] writes them
/// for the next document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Directives {
    /// `%YAML major.minor`, if the document has one.
    pub yaml_version: Option<(u32, u32)>,
    /// Each `%TAG` handle (`!e!`) and the prefix it stands for, %-escapes
    /// decoded as in tags, in the order written.
    pub tags: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Directives {
    /// Neither `%YAML` nor `%TAG`.
    pub fn is_empty(&self) -> bool {
        self.yaml_version.is_none() && self.tags.is_empty()
    }

    /// From the C accessors: `version` fills a `%YAML` version, `tag` the
    /// `%TAG` directive it's asked for, each saying whether there is one.
    ///
    /// # Safety
    /// The strings `tag` gives must be readable until it is called again.
    unsafe fn read(
        version: impl FnOnce(&mut c_int, &mut c_int) -> bool,
        mut tag: impl FnMut(c_int, &mut sys::oyl_str, &mut sys::oyl_str) -> bool,
    ) -> Self {
        let part = |n: c_int| u32::try_from(n).unwrap_or(0);
        let (mut major, mut minor) = (0, 0);
        let yaml_version = version(&mut major, &mut minor).then(|| (part(major), part(minor)));
        let mut tags = Vec::new();
        let none = sys::oyl_str { data: std::ptr::null(), len: 0 };
        let (mut handle, mut prefix) = (none, none);
        let mut i = 0;
        while tag(i, &mut handle, &mut prefix) {
            tags.push((bytes(handle).to_vec(), bytes(prefix).to_vec()));
            i += 1;
        }
        Directives { yaml_version, tags }
    }
}

// ── Callbacks ───────────────────────────────────────────────

// A panic in a Rust callback that Oyl calls (a custom type's parser, a
// warning handler) can't unwind through C: the callback catches it and
// leaves it here, and the call into C that led to it resumes it on return.

thread_local! {
    static PANIC: Cell<Option<Box<dyn Any + Send>>> = const { Cell::new(None) };
}

/// Whether a callback was ever set, so that calls into C needn't look for
/// a panic otherwise.
static CALLBACKS: AtomicBool = AtomicBool::new(false);

/// Run a callback for C: its result, or `fallback` if it panicked, the
/// panic kept for [`resume_panic`].
fn guard<R>(fallback: R, f: impl FnOnce() -> R) -> R {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(payload) => {
            PANIC.with(|p| {
                let first = p.take();
                p.set(first.or(Some(payload)));
            });
            fallback
        }
    }
}

/// After a call into C that may have called back into Rust: a callback's
/// panic goes on from here.
fn resume_panic() {
    if CALLBACKS.load(Ordering::Relaxed) {
        if let Some(payload) = PANIC.with(Cell::take) {
            std::panic::resume_unwind(payload);
        }
    }
}

// ── Values ──────────────────────────────────────────────────

/// A scalar's value, as a schema types it.
#[derive(Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Value<'a> {
    /// `null`, `~`, or nothing.
    Null,
    /// `true` or `false` (and, in YAML 1.1 and go-yaml v2, `yes`, `on`...).
    Bool(bool),
    /// An integer that fits `i64`.
    Int(i64),
    /// An integer above `i64::MAX`.
    Uint(u64),
    /// A floating-point number, `.inf` and `.nan` included.
    Float(f64),
    /// Text: a string, a timestamp, or an integer beyond 64 bits.
    Str(&'a [u8]),
}

impl fmt::Debug for Value<'_> {
    /// Text shows as text (lossily, if it isn't UTF-8), not as bytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Value::Null => f.write_str("Null"),
            Value::Bool(b) => f.debug_tuple("Bool").field(&b).finish(),
            Value::Int(i) => f.debug_tuple("Int").field(&i).finish(),
            Value::Uint(u) => f.debug_tuple("Uint").field(&u).finish(),
            Value::Float(x) => f.debug_tuple("Float").field(&x).finish(),
            Value::Str(s) => f.debug_tuple("Str").field(&String::from_utf8_lossy(s)).finish(),
        }
    }
}

impl<'a> Value<'a> {
    /// # Safety
    /// A string value must point to bytes that outlive `'a`; `text` is the
    /// scalar's text, the value of a kind this version doesn't know.
    unsafe fn from_raw(v: &sys::oyl_value, text: &'a [u8]) -> Self {
        match v.kind {
            sys::OYL_VALUE_NULL => Value::Null,
            sys::OYL_VALUE_BOOL => Value::Bool(v.as_.b),
            sys::OYL_VALUE_INT => Value::Int(v.as_.i),
            sys::OYL_VALUE_UINT => Value::Uint(v.as_.u),
            sys::OYL_VALUE_FLOAT => Value::Float(v.as_.f),
            sys::OYL_VALUE_STR => Value::Str(bytes(v.as_.s)),
            _ => Value::Str(text),
        }
    }

    /// As the C library has it; text points into the value's bytes.
    pub(crate) fn raw(&self) -> sys::oyl_value {
        let (kind, as_) = match *self {
            Value::Null => (sys::OYL_VALUE_NULL, sys::oyl_value_as { i: 0 }),
            Value::Bool(b) => (sys::OYL_VALUE_BOOL, sys::oyl_value_as { b }),
            Value::Int(i) => (sys::OYL_VALUE_INT, sys::oyl_value_as { i }),
            Value::Uint(u) => (sys::OYL_VALUE_UINT, sys::oyl_value_as { u }),
            Value::Float(f) => (sys::OYL_VALUE_FLOAT, sys::oyl_value_as { f }),
            Value::Str(s) => (sys::OYL_VALUE_STR, sys::oyl_value_as { s: raw_str(Some(s)) }),
        };
        sys::oyl_value { kind, as_ }
    }

    /// Whether it's [`Value::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// A boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// An integer that fits `i64`.
    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Value::Int(i) => Some(i),
            Value::Uint(u) => i64::try_from(u).ok(),
            _ => None,
        }
    }

    /// An integer that fits `u64`.
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::Int(i) => u64::try_from(i).ok(),
            Value::Uint(u) => Some(u),
            _ => None,
        }
    }

    /// A number as `f64`: a float, or an integer (rounded if it must be).
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Value::Float(f) => Some(f),
            Value::Int(i) => Some(i as f64),
            Value::Uint(u) => Some(u as f64),
            _ => None,
        }
    }

    /// Text as bytes.
    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        match *self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Text as UTF-8.
    pub fn as_str(&self) -> Option<Result<&'a str, Utf8Error>> {
        self.as_bytes().map(std::str::from_utf8)
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
    /// The emitter was given events out of order, a name YAML can't hold
    /// as an anchor or alias, or a directive it can't take.
    Emit,
    /// A safety limit was exceeded: the event count, nesting depth, or
    /// alias and merge expansion. See [`Parser::max_events`].
    Limit,
    /// The YAML doesn't fit the type it's deserialized into, or a value
    /// failed to serialize (serde).
    Data,
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
    /// What kind of failure it is.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Oyl's description of it, without the position.
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
///
/// The presets are `Schema<'static>`; one a program built borrows from its
/// [`CustomSchema`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Schema<'a>(NonNull<sys::oyl_schema>, PhantomData<&'a sys::oyl_schema>);

// SAFETY: a schema is never changed; a built one's parsers are Send + Sync
unsafe impl Send for Schema<'_> {}
unsafe impl Sync for Schema<'_> {}

impl Schema<'static> {
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

    /// YAML 1.1's types as PyYAML reads them: `yes`/`no`/`on`/`off`
    /// booleans (not `y`/`n`), `0b` binary, `0`-octal, base 60,
    /// underscores, floats with a dot (`.5`, not `-.5`), timestamps, `<<`
    /// as a merge.
    pub fn yaml11() -> Self {
        Schema::preset(unsafe { sys::oyl_schema_yaml11() })
    }

    /// go-yaml v2's types, as Kubernetes reads YAML: `yes`/`no`/`on`/`off`
    /// booleans, `0777` octal, `0b` binary, underscores, timestamps.
    pub fn goyaml2() -> Self {
        Schema::preset(unsafe { sys::oyl_schema_goyaml2() })
    }

    fn preset(p: *const sys::oyl_schema) -> Self {
        Schema(NonNull::new(p.cast_mut()).expect("Oyl's preset schemas are static"), PhantomData)
    }
}

impl<'a> Schema<'a> {
    /// The tag this schema gives a scalar with this value and style:
    /// quoted scalars are strings, plain ones are matched against the rules.
    pub fn resolve(&self, value: &[u8], style: ScalarStyle) -> &'a [u8] {
        // SAFETY: a schema's tags are static or in its arena, which lives for 'a
        let tag = unsafe { bytes(sys::oyl_schema_resolve(self.0.as_ptr(), raw_str(Some(value)), style.raw())) };
        resume_panic();
        tag
    }

    /// The value this schema gives a scalar written as `text` in `style`,
    /// with `tag` (`None`: untagged, so resolved first). An error if the
    /// tag's rules reject the text (`!!int abc`); a tag with no rules gives
    /// the text.
    pub fn value<'t>(&self, text: &'t [u8], style: ScalarStyle, tag: Option<&[u8]>) -> Result<Value<'t>, Error> {
        let mut v = std::mem::MaybeUninit::<sys::oyl_value>::uninit();
        let status = unsafe {
            sys::oyl_schema_value_of(self.0.as_ptr(), raw_str(Some(text)), style.raw(), raw_str(tag), v.as_mut_ptr())
        };
        resume_panic();
        check(status)?;
        // SAFETY: on success the value is set; text points into `text`
        Ok(unsafe { Value::from_raw(&v.assume_init(), text) })
    }
}

impl fmt::Debug for Schema<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Schema")
    }
}

// ── Dialects ────────────────────────────────────────────────

/// How `<<` merge keys combine with the keys written in a mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MergeOrder {
    /// `<<` is an ordinary key.
    None,
    /// Keys written in the mapping win, then the merges: a later `<<`
    /// over an earlier one, a sequence's earlier items over its later
    /// ones, as in PyYAML (YAML's merge).
    Yaml,
    /// go-yaml v2's: a merge overrides the keys written before it.
    Ordered,
}

impl MergeOrder {
    pub(crate) fn raw(self) -> sys::oyl_merge_order {
        match self {
            MergeOrder::None => sys::OYL_MERGE_NONE,
            MergeOrder::Yaml => sys::OYL_MERGE_YAML,
            MergeOrder::Ordered => sys::OYL_MERGE_ORDERED,
        }
    }
}

/// What varies between YAML worlds: a schema (which types scalars get),
/// syntax leniencies, and how `<<` merges, for one YAML version. A
/// document reads by the parser's dialect ([`Parser::dialect`]), unless
/// its `%YAML` directive names another version: `%YAML 1.1` then chooses
/// [`Dialect::yaml11`], YAML 1.1 with PyYAML's types and merge, and
/// `%YAML 1.2` [`Dialect::yaml12`].
///
/// The presets are `Dialect<'static>`; one a program built borrows from its
/// [`CustomDialect`], and a [`Tree`]'s dialect borrows from the tree (the
/// parser may have made it from a [`Schema`] alone, and the tree keeps a
/// copy), so it can't outlive the tree:
///
/// ```compile_fail
/// let parser = oyl::Parser::new("a: 1").unwrap();
/// let tree = parser.next_tree().unwrap().unwrap();
/// let dialect = tree.dialect();
/// drop(tree);
/// dialect.merge_order();
/// ```
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Dialect<'a>(NonNull<sys::oyl_dialect>, PhantomData<&'a sys::oyl_dialect>);

// SAFETY: a dialect is never changed
unsafe impl Send for Dialect<'_> {}
unsafe impl Sync for Dialect<'_> {}

impl Dialect<'static> {
    /// YAML 1.2: the core schema, YAML 1.2's syntax, YAML's merge. The
    /// default.
    pub fn yaml12() -> Self {
        Dialect::preset(unsafe { sys::oyl_dialect_yaml12() })
    }

    /// YAML 1.1 with PyYAML's types ([`Schema::yaml11`]) and YAML's
    /// merge: a document PyYAML reads gets the same values. Two
    /// exceptions: ints beyond 64 bits are text, and keys are equal as
    /// YAML has them, by tag and value, where Python's `==` also makes
    /// `1`, `1.0` and `true` one key. The syntax is YAML 1.2's, but a
    /// block scalar ending the input gets no final line feed, as in
    /// libyaml. Malformed YAML that libyaml lets through stays an error,
    /// such as a comment touching a closing quote or bracket or a block
    /// scalar's header (`"a"#c`, `[a]#c`, `|#c`), or a quoted scalar or
    /// flow collection whose later lines aren't indented past the block it
    /// sits in.
    pub fn yaml11() -> Self {
        Dialect::preset(unsafe { sys::oyl_dialect_yaml11() })
    }

    /// go-yaml v2, as Kubernetes reads YAML: its types, its syntax
    /// leniencies, and its merge order.
    pub fn goyaml2() -> Self {
        Dialect::preset(unsafe { sys::oyl_dialect_goyaml2() })
    }

    fn preset(p: *const sys::oyl_dialect) -> Self {
        Dialect(NonNull::new(p.cast_mut()).expect("Oyl's preset dialects are static"), PhantomData)
    }
}

impl<'a> Dialect<'a> {
    /// The schema that types its scalars.
    pub fn schema(&self) -> Schema<'a> {
        let s = unsafe { sys::oyl_dialect_schema(self.0.as_ptr()) };
        Schema(NonNull::new(s.cast_mut()).expect("a dialect has a schema"), PhantomData)
    }

    /// The syntax leniencies it takes on.
    pub fn syntax(&self) -> Syntax {
        Syntax::from_bits(unsafe { sys::oyl_dialect_syntax(self.0.as_ptr()) })
    }

    /// The YAML version it is a dialect of: `(1, 2)` for YAML 1.2.
    pub fn version(&self) -> (u32, u32) {
        let (mut major, mut minor) = (0, 0);
        unsafe { sys::oyl_dialect_version(self.0.as_ptr(), &mut major, &mut minor) };
        (u32::try_from(major).unwrap_or(0), u32::try_from(minor).unwrap_or(0))
    }

    /// How it applies `<<` merge keys.
    pub fn merge_order(&self) -> MergeOrder {
        match unsafe { sys::oyl_dialect_merge(self.0.as_ptr()) } {
            sys::OYL_MERGE_NONE => MergeOrder::None,
            sys::OYL_MERGE_ORDERED => MergeOrder::Ordered,
            _ => MergeOrder::Yaml,
        }
    }
}

impl fmt::Debug for Dialect<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = if *self == Dialect::yaml12() {
            "yaml12"
        } else if *self == Dialect::yaml11() {
            "yaml11"
        } else if *self == Dialect::goyaml2() {
            "goyaml2"
        } else {
            "custom"
        };
        write!(f, "Dialect({name})")
    }
}

// ── JSON ────────────────────────────────────────────────────

/// How [`Parser::next_json`] writes a document as JSON.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonOptions<'a> {
    /// The schema that types scalars; `None` types them by the document's
    /// dialect, whose merge order then applies.
    pub schema: Option<Schema<'a>>,
    /// Expand `<<` merge keys (with a schema; a dialect decides itself).
    pub merge: bool,
    /// With `merge`, go-yaml v2's order: a merge overrides the keys before it.
    pub merge_ordered: bool,
    /// A key set twice is an error, instead of keeping its last value.
    pub strict: bool,
}

impl JsonOptions<'static> {
    /// As Kubernetes reads YAML: byte for byte what sigs.k8s.io/yaml's
    /// `YAMLToJSON` writes.
    pub fn kubernetes() -> Self {
        JsonOptions { schema: Some(Schema::goyaml2()), merge: true, merge_ordered: true, strict: false }
    }
}

impl JsonOptions<'_> {
    fn flags(&self) -> std::ffi::c_uint {
        let mut f = 0;
        if self.merge { f |= sys::OYL_JSON_MERGE; }
        if self.merge_ordered { f |= sys::OYL_JSON_MERGE_ORDER; }
        if self.strict { f |= sys::OYL_JSON_STRICT; }
        f
    }
}

// ── Warnings ────────────────────────────────────────────────

/// What a [`Warning`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WarningKind {
    /// A `%YAML` 1.x version other than 1.1 or 1.2, read as YAML 1.2.
    YamlVersion,
    /// A `%TAG !!` prefix no dialect was given for
    /// ([`Parser::dialect_for_tag_prefix`]): untagged values keep the
    /// standard types.
    Dialect,
    /// The dialect accepted what YAML 1.2 doesn't (go-yaml v2's `\'`, say).
    Leniency,
    /// In go-yaml v2's merge order, a merge overrode a key written before
    /// it, which YAML's merge would keep.
    MergeOrder,
    /// A kind this version doesn't know.
    Other,
}

impl WarningKind {
    fn from_raw(code: sys::oyl_warning) -> Self {
        match code {
            sys::OYL_WARN_YAML_VERSION => WarningKind::YamlVersion,
            sys::OYL_WARN_DIALECT => WarningKind::Dialect,
            sys::OYL_WARN_LENIENCY => WarningKind::Leniency,
            sys::OYL_WARN_MERGE_ORDER => WarningKind::MergeOrder,
            _ => WarningKind::Other,
        }
    }
}

/// Something the parser read that YAML 1.2 would read otherwise, or not at
/// all: see [`Parser::on_warning`]. Parsing goes on.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Warning {
    /// What it is about.
    pub kind: WarningKind,
    /// Oyl's description of it.
    pub message: String,
    /// Where in the input.
    pub mark: Mark,
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at line {}, column {}", self.message, self.mark.line, self.mark.column)
    }
}

type WarnFn = Box<dyn FnMut(&Warning) + Send>;

/// Oyl calls this for each warning, with the parser's handler.
unsafe extern "C" fn warn_handler(user: *mut c_void, code: sys::oyl_warning, mark: sys::oyl_mark, message: *const c_char) {
    // SAFETY: `user` is the handler the parser owns, called only from
    // within the parser's own calls; the message is a C string for this call
    let f = &mut *user.cast::<WarnFn>();
    let warning = Warning {
        kind: WarningKind::from_raw(code),
        message: c_string(message).unwrap_or_default(),
        mark: mark.into(),
    };
    guard((), || f(&warning));
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
///
/// The parser's lifetime `'i` is that of its input and of any schema or
/// dialect given to it: trees read with it keep pointers to them.
pub struct Parser<'i> {
    arena: NonNull<sys::oyl_arena>,
    parser: NonNull<sys::oyl_parser>,
    state: Cell<State>,
    warn: Option<NonNull<WarnFn>>,
    _input: PhantomData<&'i [u8]>,
}

// SAFETY: the parser, its arena and its warning handler (Send) belong to
// this value alone, and the input, schemas and dialects are shared
// immutably; next_event takes &self, so it isn't Sync.
unsafe impl Send for Parser<'_> {}

impl<'i> Parser<'i> {
    /// A parser over `input` (a `str`, `[u8]`, `String` or `Vec<u8>`).
    pub fn new<T: AsRef<[u8]> + ?Sized>(input: &'i T) -> Result<Self, Error> {
        let input = input.as_ref();
        let arena = NonNull::new(unsafe { sys::oyl_arena_new(input.len().clamp(4096, 1 << 20)) })
            .ok_or_else(Error::memory)?;
        let parser = unsafe { sys::oyl_parser_new(input.as_ptr().cast(), input.len(), arena.as_ptr()) };
        match NonNull::new(parser) {
            Some(parser) => Ok(Parser { arena, parser, state: Cell::new(State::Parsing), warn: None, _input: PhantomData }),
            None => {
                unsafe { sys::oyl_arena_free(arena.as_ptr()) };
                Err(Error::memory())
            }
        }
    }

    /// Expand merge keys (`<<: *base`), in YAML's merge order. Off by
    /// default. A merge value must be a mapping, an alias to one, or a
    /// sequence of those, also through an alias.
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

    /// Resolve the tags of untagged nodes with this schema; without a
    /// [`dialect`](Parser::dialect), documents are typed by it too.
    pub fn schema(&mut self, schema: Schema<'i>) -> &mut Self {
        unsafe { sys::oyl_parser_set_schema(self.parser.as_ptr(), schema.0.as_ptr()) };
        self
    }

    /// Read documents by this dialect, unless one's `%YAML` directive names
    /// another version: `%YAML 1.1` then chooses [`Dialect::yaml11`] and
    /// `%YAML 1.2` [`Dialect::yaml12`] (other 1.x versions read as 1.2).
    /// go-yaml v2's dialect, of YAML 1.1, refuses any `%YAML` but 1.1, as
    /// go-yaml v2 does. Without a dialect, the parser reads YAML 1.2 by
    /// its [`schema`](Parser::schema) if set. Trees and JSON type scalars
    /// by the document's dialect.
    pub fn dialect(&mut self, dialect: Dialect<'i>) -> &mut Self {
        unsafe { sys::oyl_parser_set_dialect(self.parser.as_ptr(), dialect.0.as_ptr()) };
        self
    }

    /// Let documents choose `dialect` with `%YAML major.minor`: it comes
    /// before the parser's dialect and the built-in one for that version.
    /// Given again for a version, it replaces the one before. An
    /// [`ErrorKind::Input`] error for a version not 1.x, and
    /// [`ErrorKind::Limit`] past 8 versions.
    pub fn dialect_for_version(&mut self, version: (u32, u32), dialect: Dialect<'i>) -> Result<&mut Self, Error> {
        let v = CString::new(format!("{}.{}", version.0, version.1)).expect("digits and a dot");
        check(unsafe { sys::oyl_parser_add_dialect(self.parser.as_ptr(), dialect.0.as_ptr(), v.as_ptr(), std::ptr::null()) })?;
        Ok(self)
    }

    /// Let documents choose `dialect` with `%TAG !! prefix`, before
    /// anything their `%YAML` says: a document can then name a project's
    /// own types as `!!name`. The document's prefix is compared %-escapes
    /// decoded. Given again for a prefix, it replaces the one before. An
    /// [`ErrorKind::Input`] error for a prefix of 256 bytes or more, or
    /// holding a NUL byte, and [`ErrorKind::Limit`] past 8 prefixes.
    pub fn dialect_for_tag_prefix(&mut self, prefix: &str, dialect: Dialect<'i>) -> Result<&mut Self, Error> {
        let prefix = CString::new(prefix).map_err(|_| Error {
            kind: ErrorKind::Input,
            message: "a %TAG prefix holding a NUL byte".into(),
            mark: None,
        })?;
        check(unsafe { sys::oyl_parser_add_dialect(self.parser.as_ptr(), dialect.0.as_ptr(), std::ptr::null(), prefix.as_ptr()) })?;
        Ok(self)
    }

    /// Call `f` with each [`Warning`]: what the parser read, but YAML 1.2
    /// would read otherwise (a `%YAML` version it reads as another, a
    /// `%TAG !!` prefix with no dialect given for it, a leniency of the
    /// dialect, a merge go-yaml v2's order reads otherwise). Warnings don't
    /// stop parsing. A panic in `f` goes on from the call that was reading.
    ///
    /// ```
    /// use std::sync::{Arc, Mutex};
    /// use oyl::{Parser, WarningKind};
    ///
    /// let seen = Arc::new(Mutex::new(Vec::new()));
    /// let mut parser = Parser::new("%YAML 1.3\n--- a\n")?;
    /// let log = Arc::clone(&seen);
    /// parser.on_warning(move |w| log.lock().unwrap().push(w.kind));
    /// parser.next_tree()?;
    /// assert_eq!(*seen.lock().unwrap(), [WarningKind::YamlVersion]);
    /// # Ok::<(), oyl::Error>(())
    /// ```
    pub fn on_warning(&mut self, f: impl FnMut(&Warning) + Send + 'static) -> &mut Self {
        let handler: WarnFn = Box::new(f);
        let user = NonNull::from(Box::leak(Box::new(handler)));
        CALLBACKS.store(true, Ordering::Relaxed);
        unsafe { sys::oyl_parser_set_warn(self.parser.as_ptr(), Some(warn_handler), user.as_ptr().cast()) };
        if let Some(old) = self.warn.replace(user) {
            // SAFETY: leaked from a box above, and no longer Oyl's
            drop(unsafe { Box::from_raw(old.as_ptr()) });
        }
        self
    }

    /// The dialect of the document whose
    /// [`DocumentStart`](EventKind::DocumentStart) came last: ask when that
    /// event arrives (the parser may read ahead, so by the document's end it
    /// may be the next one's). Before the first document, YAML 1.2's.
    pub fn document_dialect(&self) -> Dialect<'_> {
        let d = unsafe { sys::oyl_parser_document_dialect(self.parser.as_ptr()) };
        NonNull::new(d.cast_mut()).map_or(Dialect::yaml12(), |d| Dialect(d, PhantomData))
    }

    /// The next document as JSON; `Ok(None)` once the stream has ended.
    /// Keys are sorted, a key set twice keeps its last value, aliases are
    /// expanded, and numbers and strings are written as Go's encoding/json
    /// writes them. Leave [`merge`](Parser::merge) and
    /// [`resolve`](Parser::resolve) off: this does both.
    pub fn next_json(&self, options: &JsonOptions<'_>) -> Result<Option<&str>, Error> {
        if self.state.get() != State::Parsing {
            return Ok(None);
        }
        let mut json = sys::oyl_str { data: std::ptr::null(), len: 0 };
        let schema = options.schema.map_or(std::ptr::null(), |s| s.0.as_ptr().cast_const());
        let status = unsafe { sys::oyl_parse_json(self.parser.as_ptr(), schema, options.flags(), &mut json) };
        resume_panic();
        if status != sys::OYL_OK {
            self.state.set(State::Failed);
            return Err(self.error(status));
        }
        // SAFETY: the JSON is in the parser's arena, which outlives the borrow
        let out = unsafe { bytes(json) };
        if out.is_empty() {
            self.state.set(State::Done);
            return Ok(None);
        }
        std::str::from_utf8(out).map(Some).map_err(|_| Error {
            kind: ErrorKind::Other,
            message: "JSON output is not UTF-8".into(),
            mark: None,
        })
    }

    /// The next event; `Ok(None)` once the stream has ended.
    pub fn next_event(&self) -> Result<Option<Event<'_>>, Error> {
        if self.state.get() != State::Parsing {
            return Ok(None);
        }
        let mut raw: *const sys::oyl_event = std::ptr::null();
        let status = unsafe { sys::oyl_parse_next(self.parser.as_ptr(), &mut raw) };
        resume_panic();
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

    /// The directives of the document whose
    /// [`DocumentStart`](EventKind::DocumentStart) came last: ask when that
    /// event arrives (the parser may read ahead, so by the document's end
    /// they may be the next one's). To re-emit the document with them,
    /// give them to [`Emitter::directives`] before its `DocumentStart`, or
    /// use [`Emitter::reemit`].
    pub fn directives(&self) -> Directives {
        let p = self.parser.as_ptr();
        // SAFETY: the strings are the parser's until it reads on, and are
        // copied before then
        unsafe {
            Directives::read(
                |major, minor| sys::oyl_parser_yaml_version(p, major, minor),
                |i, handle, prefix| sys::oyl_parser_tag_directive(p, i, handle, prefix),
            )
        }
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
        if let Some(handler) = self.warn.take() {
            // SAFETY: leaked from a box in on_warning; the parser is gone
            drop(unsafe { Box::from_raw(handler.as_ptr()) });
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
/// Its output reads back as the events it was given: scalars are quoted
/// when plain style would read back differently, a document gets `---`
/// where YAML needs one, and so on. Events in another order (two roots, a
/// mapping ended after a key, an end that doesn't match its start), anchor
/// or alias names YAML can't hold, and directives it can't take are
/// [`ErrorKind::Emit`] errors. An error is final: every later call returns
/// it, and the output so far may be incomplete.
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

    /// Write in this style (default [`EmitStyle::Block`]).
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

    /// Begin the output; call it first.
    pub fn stream_start(&mut self) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_stream_start(self.e()) })
    }

    /// End the output; call it last.
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

    /// End the mapping begun last.
    pub fn mapping_end(&mut self) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_mapping_end(self.e()) })
    }

    /// Start a sequence; `flow` writes it in flow style (`[...]`).
    pub fn sequence_start(&mut self, anchor: Option<&str>, tag: Option<&str>, flow: bool) -> Result<(), Error> {
        self.raw_sequence_start(anchor.map(str::as_bytes), tag.map(str::as_bytes), flow)
    }

    /// End the sequence begun last.
    pub fn sequence_end(&mut self) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_sequence_end(self.e()) })
    }

    /// Emit a parsed event, e.g. to reformat a document. A document's
    /// directives aren't among its events: give the parser's
    /// [`directives`](Parser::directives) at its `DocumentStart`, or use
    /// [`reemit`](Emitter::reemit).
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

    /// Re-emit the rest of a parser's events, each document with its
    /// directives: the input reformatted ([`style`](Emitter::style),
    /// [`indent`](Emitter::indent)).
    pub fn reemit(&mut self, parser: &Parser<'_>) -> Result<(), Error> {
        for event in parser.events() {
            let event = event?;
            if event.kind == EventKind::DocumentStart {
                self.directives(&parser.directives())?;
            }
            self.event(&event)?;
        }
        Ok(())
    }

    /// Emit a tree as one document, with its `%YAML` and `%TAG` directives,
    /// and `---` and `...` where they were written: what emitting its
    /// document's events would write. Directives given for it first
    /// ([`directives`](Emitter::directives)) take the place of its own: a
    /// `%YAML` given replaces the tree's, and a `%TAG` given replaces the
    /// tree's for the same handle.
    pub fn tree(&mut self, tree: &Tree<'_>) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_tree(self.e(), tree.raw()) })
    }

    /// Directives for the next document, given between documents (before
    /// its [`document_start`](Emitter::document_start)). The document then
    /// starts with `---`, and the one before it ends with `...` if it needs
    /// to. Its tags are written with its handles where they fit (`!e!foo`),
    /// and with `!` and `!!` only as it defines them, otherwise verbatim.
    /// The emitter writes no `%YAML` of its own: a document without one is
    /// YAML 1.2 to a YAML 1.2 reader.
    pub fn directives(&mut self, directives: &Directives) -> Result<(), Error> {
        if let Some((major, minor)) = directives.yaml_version {
            self.yaml_directive(major, minor)?;
        }
        for (handle, prefix) in &directives.tags {
            self.raw_tag_directive(handle, prefix)?;
        }
        Ok(())
    }

    /// `%YAML major.minor` for the next document (see
    /// [`directives`](Emitter::directives)). An [`ErrorKind::Emit`] error
    /// inside a document, for a version not 1.x, or for a second one.
    pub fn yaml_directive(&mut self, major: u32, minor: u32) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_yaml_directive(self.e(), clamp_int(major), clamp_int(minor)) })
    }

    /// `%TAG handle prefix` for the next document (see
    /// [`directives`](Emitter::directives)). The prefix is as tags have it:
    /// what isn't a URI character is %-escaped when written. An
    /// [`ErrorKind::Emit`] error inside a document; for a handle not `!`,
    /// `!!` or `!name!` (letters, digits and `-`), or given twice for the
    /// document; for an empty prefix; and past what Oyl's parser reads
    /// back: more than 8 handles, a handle of 16 bytes or more, or a prefix
    /// of 256 bytes or more as written.
    pub fn tag_directive(&mut self, handle: &str, prefix: &str) -> Result<(), Error> {
        self.raw_tag_directive(handle.as_bytes(), prefix.as_bytes())
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

    fn raw_tag_directive(&mut self, handle: &[u8], prefix: &[u8]) -> Result<(), Error> {
        check(unsafe { sys::oyl_emit_tag_directive(self.e(), raw_str(Some(handle)), raw_str(Some(prefix))) })
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
