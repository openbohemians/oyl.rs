//! Raw FFI bindings to [Oyl](https://github.com/openbohemians/oyl), the
//! optimized YAML library: a YAML 1.2 parser and emitter written in C11.
//!
//! These mirror `oyl.h` one to one; the [`oyl`](https://docs.rs/oyl) crate
//! is the safe API. The C library is compiled from the sources vendored in
//! this crate (`oyl/COMMIT` names the Oyl commit), so nothing needs to be
//! installed.
//!
//! C enums are integer types with a constant per value rather than Rust
//! enums: reading a value a Rust enum lacks is undefined behavior, and a
//! later Oyl may add values. `oyl_token` and `oyl_event` are only ever
//! allocated by the library and handed out by const pointer; never create
//! one (fields may be appended in later versions). A tree's arrays
//! (`oyl_node`, `oyl_node_props`, `oyl_pair`) have layouts fixed for good,
//! and may be read in place.

#![allow(non_camel_case_types)]
#![cfg_attr(not(test), no_std)]

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::marker::{PhantomData, PhantomPinned};

// ── Value types ─────────────────────────────────────────────

/// Non-owning reference to a UTF-8 string, pointing into the input buffer
/// or arena memory: valid until the arena is freed or reset.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct oyl_str {
    pub data: *const c_char,
    pub len: usize,
}

/// Byte offset, 1-based line and 1-based column (in bytes) in the input.
#[repr(C)]
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct oyl_mark {
    pub offset: usize,
    pub line: usize,
    pub col: usize,
}

// ── Status codes ────────────────────────────────────────────

pub type oyl_status = c_uint;
pub const OYL_OK: oyl_status = 0;
pub const OYL_ERR_MEMORY: oyl_status = 1;
pub const OYL_ERR_INPUT: oyl_status = 2;
pub const OYL_ERR_SCAN: oyl_status = 3;
pub const OYL_ERR_PARSE: oyl_status = 4;
pub const OYL_ERR_EMIT: oyl_status = 5;
pub const OYL_ERR_LIMIT: oyl_status = 6;

// ── Tokens (scanner output) ─────────────────────────────────

pub type oyl_token_type = c_uint;
pub const OYL_TOK_NONE: oyl_token_type = 0;
pub const OYL_TOK_STREAM_START: oyl_token_type = 1;
pub const OYL_TOK_STREAM_END: oyl_token_type = 2;
pub const OYL_TOK_DOC_START: oyl_token_type = 3;
pub const OYL_TOK_DOC_END: oyl_token_type = 4;
pub const OYL_TOK_BLOCK_SEQ_ENTRY: oyl_token_type = 5;
pub const OYL_TOK_BLOCK_MAP_KEY: oyl_token_type = 6;
pub const OYL_TOK_BLOCK_MAP_VALUE: oyl_token_type = 7;
pub const OYL_TOK_FLOW_SEQ_START: oyl_token_type = 8;
pub const OYL_TOK_FLOW_SEQ_END: oyl_token_type = 9;
pub const OYL_TOK_FLOW_MAP_START: oyl_token_type = 10;
pub const OYL_TOK_FLOW_MAP_END: oyl_token_type = 11;
pub const OYL_TOK_FLOW_ENTRY: oyl_token_type = 12;
pub const OYL_TOK_SCALAR: oyl_token_type = 13;
pub const OYL_TOK_TAG: oyl_token_type = 14;
pub const OYL_TOK_ANCHOR: oyl_token_type = 15;
pub const OYL_TOK_ALIAS: oyl_token_type = 16;
pub const OYL_TOK_DIRECTIVE: oyl_token_type = 17;

pub type oyl_scalar_style = c_uint;
pub const OYL_SCALAR_PLAIN: oyl_scalar_style = 0;
pub const OYL_SCALAR_SINGLE_QUOTED: oyl_scalar_style = 1;
pub const OYL_SCALAR_DOUBLE_QUOTED: oyl_scalar_style = 2;
pub const OYL_SCALAR_LITERAL: oyl_scalar_style = 3;
pub const OYL_SCALAR_FOLDED: oyl_scalar_style = 4;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct oyl_token {
    pub type_: oyl_token_type,
    pub value: oyl_str,
    pub scalar_style: oyl_scalar_style,
    pub start: oyl_mark,
    pub end: oyl_mark,
}

// ── Events (parser output) ──────────────────────────────────

pub type oyl_event_type = c_uint;
pub const OYL_EVT_NONE: oyl_event_type = 0;
pub const OYL_EVT_STREAM_START: oyl_event_type = 1;
pub const OYL_EVT_STREAM_END: oyl_event_type = 2;
pub const OYL_EVT_DOC_START: oyl_event_type = 3;
pub const OYL_EVT_DOC_END: oyl_event_type = 4;
pub const OYL_EVT_MAPPING_START: oyl_event_type = 5;
pub const OYL_EVT_MAPPING_END: oyl_event_type = 6;
pub const OYL_EVT_SEQUENCE_START: oyl_event_type = 7;
pub const OYL_EVT_SEQUENCE_END: oyl_event_type = 8;
pub const OYL_EVT_SCALAR: oyl_event_type = 9;
pub const OYL_EVT_ALIAS: oyl_event_type = 10;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct oyl_event {
    pub type_: oyl_event_type,
    /// Scalar value or alias name.
    pub value: oyl_str,
    pub anchor: oyl_str,
    pub tag: oyl_str,
    pub scalar_style: oyl_scalar_style,
    /// True for implicit document starts and ends.
    pub implicit: bool,
    /// True for flow collections.
    pub flow: bool,
    pub start: oyl_mark,
    pub end: oyl_mark,
}

// ── Opaque types ────────────────────────────────────────────

macro_rules! opaque {
    ($($name:ident),*) => {$(
        #[repr(C)]
        pub struct $name {
            _private: [u8; 0],
            _marker: PhantomData<(*mut u8, PhantomPinned)>,
        }
    )*};
}
opaque!(oyl_arena, oyl_scanner, oyl_parser, oyl_emitter, oyl_schema, oyl_schema_builder, oyl_dialect, oyl_tree);

pub type oyl_match_type = c_uint;
pub const OYL_MATCH_EXACT: oyl_match_type = 0;
pub const OYL_MATCH_ICASE: oyl_match_type = 1;
pub const OYL_MATCH_BUILTIN: oyl_match_type = 2;

pub type oyl_emit_style = c_uint;
pub const OYL_EMIT_BLOCK: oyl_emit_style = 0;
pub const OYL_EMIT_FLOW: oyl_emit_style = 1;
pub const OYL_EMIT_MINIMAL: oyl_emit_style = 2;

// ── Scalar values ───────────────────────────────────────────

/// The kind of a scalar's value. Kinds may be added at the end; treat one
/// you don't know as text.
pub type oyl_value_kind = c_uint;
pub const OYL_VALUE_NULL: oyl_value_kind = 0;
pub const OYL_VALUE_BOOL: oyl_value_kind = 1;
/// Fits `i64`.
pub const OYL_VALUE_INT: oyl_value_kind = 2;
/// Above `i64::MAX`, fits `u64`.
pub const OYL_VALUE_UINT: oyl_value_kind = 3;
pub const OYL_VALUE_FLOAT: oyl_value_kind = 4;
/// Text: strings, timestamps, an int beyond 64 bits.
pub const OYL_VALUE_STR: oyl_value_kind = 5;

#[repr(C)]
#[derive(Copy, Clone)]
pub union oyl_value_as {
    pub b: bool,
    pub i: i64,
    pub u: u64,
    pub f: f64,
    pub s: oyl_str,
}

/// A scalar's value. `as_.s` points into the scalar's text.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct oyl_value {
    pub kind: oyl_value_kind,
    pub as_: oyl_value_as,
}

impl core::fmt::Debug for oyl_value {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("oyl_value").field("kind", &self.kind).finish_non_exhaustive()
    }
}

/// A custom scalar parser: accept `text` by filling `out` and returning
/// true, or decline by returning false.
pub type oyl_scalar_parser = Option<unsafe extern "C" fn(text: oyl_str, out: *mut oyl_value, user: *mut c_void) -> bool>;

/// Options for the built-in int parser (decimal digits are always accepted).
pub const OYL_INT_SIGN: c_uint = 1 << 0;
pub const OYL_INT_HEX: c_uint = 1 << 1;
pub const OYL_INT_OCT: c_uint = 1 << 2;
pub const OYL_INT_OCT_ZERO: c_uint = 1 << 3;
pub const OYL_INT_BIN: c_uint = 1 << 4;
pub const OYL_INT_PREFIX_CASE: c_uint = 1 << 5;
pub const OYL_INT_SIGN_PREFIX: c_uint = 1 << 6;
pub const OYL_INT_UNDERSCORE: c_uint = 1 << 7;
pub const OYL_INT_RANGE: c_uint = 1 << 8;
pub const OYL_INT_DIGIT_UNDERSCORE: c_uint = 1 << 9;
pub const OYL_INT_BASE60: c_uint = 1 << 10;
pub const OYL_INT_CORE: c_uint = OYL_INT_SIGN | OYL_INT_HEX | OYL_INT_OCT | OYL_INT_SIGN_PREFIX;
pub const OYL_INT_YAML11: c_uint = OYL_INT_SIGN | OYL_INT_HEX | OYL_INT_OCT_ZERO | OYL_INT_BIN
    | OYL_INT_SIGN_PREFIX | OYL_INT_DIGIT_UNDERSCORE | OYL_INT_BASE60;

/// Options for the built-in float parser.
pub const OYL_FLOAT_SPECIALS: c_uint = 1 << 0;
pub const OYL_FLOAT_DIGITS: c_uint = 1 << 1;
pub const OYL_FLOAT_UNDERSCORE: c_uint = 1 << 2;
pub const OYL_FLOAT_RANGE: c_uint = 1 << 3;
pub const OYL_FLOAT_NEED_DOT: c_uint = 1 << 4;
pub const OYL_FLOAT_EXP_SIGN: c_uint = 1 << 5;
pub const OYL_FLOAT_DIGIT_UNDERSCORE: c_uint = 1 << 6;
pub const OYL_FLOAT_BASE60: c_uint = 1 << 7;
pub const OYL_FLOAT_DOT_NO_SIGN: c_uint = 1 << 8;
pub const OYL_FLOAT_CORE: c_uint = OYL_FLOAT_SPECIALS | OYL_FLOAT_DIGITS;
pub const OYL_FLOAT_YAML11: c_uint = OYL_FLOAT_SPECIALS | OYL_FLOAT_NEED_DOT | OYL_FLOAT_EXP_SIGN
    | OYL_FLOAT_DIGIT_UNDERSCORE | OYL_FLOAT_BASE60 | OYL_FLOAT_DOT_NO_SIGN;

// ── Dialects and warnings ───────────────────────────────────

/// Syntax leniencies a dialect may take on.
pub const OYL_SYNTAX_SQUOTE_ESCAPE: c_uint = 1 << 0;
pub const OYL_SYNTAX_EOF_NO_BREAK: c_uint = 1 << 1;
pub const OYL_SYNTAX_YAML11_ONLY: c_uint = 1 << 2;
/// go-yaml v2's leniencies.
pub const OYL_SYNTAX_GOYAML2: c_uint = OYL_SYNTAX_SQUOTE_ESCAPE | OYL_SYNTAX_EOF_NO_BREAK | OYL_SYNTAX_YAML11_ONLY;

/// How `<<` merge keys combine with the keys written in a mapping.
pub type oyl_merge_order = c_uint;
pub const OYL_MERGE_NONE: oyl_merge_order = 0;
pub const OYL_MERGE_YAML: oyl_merge_order = 1;
pub const OYL_MERGE_ORDERED: oyl_merge_order = 2;

/// What a warning is about. Codes may be added at the end.
pub type oyl_warning = c_uint;
pub const OYL_WARN_YAML_VERSION: oyl_warning = 1;
pub const OYL_WARN_DIALECT: oyl_warning = 2;
pub const OYL_WARN_LENIENCY: oyl_warning = 3;
pub const OYL_WARN_MERGE_ORDER: oyl_warning = 4;

pub type oyl_warn_fn = Option<unsafe extern "C" fn(user: *mut c_void, code: oyl_warning, mark: oyl_mark, message: *const c_char)>;

// ── JSON ────────────────────────────────────────────────────

pub const OYL_JSON_MERGE: c_uint = 1 << 0;
pub const OYL_JSON_MERGE_ORDER: c_uint = 1 << 1;
pub const OYL_JSON_STRICT: c_uint = 1 << 2;
pub const OYL_JSON_KUBERNETES: c_uint = OYL_JSON_MERGE | OYL_JSON_MERGE_ORDER;

// ── Trees ───────────────────────────────────────────────────

/// No node: what a lookup returns when it finds nothing.
pub const OYL_NO_NODE: u32 = u32::MAX;

/// What a node is (its `kind` byte).
pub type oyl_node_kind = c_uint;
pub const OYL_NODE_SCALAR: oyl_node_kind = 1;
pub const OYL_NODE_SEQUENCE: oyl_node_kind = 2;
pub const OYL_NODE_MAPPING: oyl_node_kind = 3;
pub const OYL_NODE_ALIAS: oyl_node_kind = 4;

/// `oyl_node` flags.
pub const OYL_NODE_POOLED: u8 = 1 << 0;
pub const OYL_NODE_KEY: u8 = 1 << 1;
pub const OYL_NODE_FLOW: u8 = 1 << 2;

/// A node: 32 bytes, laid out for good. Positions are offsets in the input
/// from the document's start (`oyl_tree_base`), except text in the pool.
#[repr(C)]
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct oyl_node {
    /// Scalar: where its text starts; alias: its anchor's name.
    pub text: u32,
    /// Scalar, alias: the text's length; sequence: its items; mapping: its pairs.
    pub len: u32,
    /// The nodes in its subtree, itself included.
    pub span: u32,
    /// How far back its parent is (0: the root).
    pub parent: u32,
    /// Its tag and anchor, an index into `oyl_tree_props` (0: none); an
    /// alias: the node it refers to.
    pub props: u32,
    pub start: u32,
    pub end: u32,
    /// `oyl_node_kind`.
    pub kind: u8,
    /// A scalar's `oyl_scalar_style`.
    pub style: u8,
    /// An untagged plain scalar's type in the document's schema.
    pub type_: u8,
    /// `OYL_NODE_` flags.
    pub flags: u8,
}

/// A node's tag and anchor, both in the pool (length 0: none).
#[repr(C)]
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct oyl_node_props {
    pub tag: u32,
    pub tag_len: u32,
    pub anchor: u32,
    pub anchor_len: u32,
}

/// A mapping's key and its value.
#[repr(C)]
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct oyl_pair {
    pub key: u32,
    pub value: u32,
}

/// `oyl_node_entries` flag: a key written twice is an error.
pub const OYL_ENTRIES_STRICT: c_uint = 1 << 0;

// ── Functions ───────────────────────────────────────────────

extern "C" {
    pub static OYL_TAG_NULL: oyl_str;
    pub static OYL_TAG_BOOL: oyl_str;
    pub static OYL_TAG_INT: oyl_str;
    pub static OYL_TAG_FLOAT: oyl_str;
    pub static OYL_TAG_STR: oyl_str;
    pub static OYL_TAG_SEQ: oyl_str;
    pub static OYL_TAG_MAP: oyl_str;
    pub static OYL_TAG_MERGE: oyl_str;
    pub static OYL_TAG_TIMESTAMP: oyl_str;
    pub static OYL_TAG_VALUE: oyl_str;
    pub static OYL_TAG_BINARY: oyl_str;

    pub fn oyl_arena_new(initial_cap: usize) -> *mut oyl_arena;
    pub fn oyl_arena_alloc(a: *mut oyl_arena, size: usize, align: usize) -> *mut c_void;
    pub fn oyl_arena_dup(a: *mut oyl_arena, src: *const c_char, len: usize) -> *mut c_char;
    pub fn oyl_arena_reset(a: *mut oyl_arena);
    pub fn oyl_arena_free(a: *mut oyl_arena);

    pub fn oyl_read_file(path: *const c_char, a: *mut oyl_arena) -> oyl_str;

    pub fn oyl_scanner_new(input: *const c_char, len: usize, a: *mut oyl_arena) -> *mut oyl_scanner;
    pub fn oyl_scan_next(s: *mut oyl_scanner, tok: *mut *const oyl_token) -> oyl_status;
    pub fn oyl_scanner_error(s: *mut oyl_scanner) -> *const c_char;
    pub fn oyl_scanner_error_mark(s: *mut oyl_scanner) -> oyl_mark;
    pub fn oyl_scanner_free(s: *mut oyl_scanner);

    pub fn oyl_schema_failsafe() -> *const oyl_schema;
    pub fn oyl_schema_json() -> *const oyl_schema;
    pub fn oyl_schema_core() -> *const oyl_schema;
    pub fn oyl_schema_resolve(schema: *const oyl_schema, value: oyl_str, style: oyl_scalar_style) -> oyl_str;

    pub fn oyl_schema_builder_new(a: *mut oyl_arena) -> *mut oyl_schema_builder;
    pub fn oyl_schema_builder_add(b: *mut oyl_schema_builder, match_: oyl_match_type, pattern: *const c_char, tag: oyl_str);
    pub fn oyl_schema_builder_add_bools(
        b: *mut oyl_schema_builder,
        true_terms: *mut *const c_char,
        ntrue: c_int,
        false_terms: *mut *const c_char,
        nfalse: c_int,
    );
    pub fn oyl_schema_builder_add_nulls(b: *mut oyl_schema_builder, terms: *mut *const c_char, nterms: c_int);
    pub fn oyl_schema_builder_add_int(b: *mut oyl_schema_builder);
    pub fn oyl_schema_builder_add_float(b: *mut oyl_schema_builder);
    pub fn oyl_schema_builder_finish(b: *mut oyl_schema_builder) -> *const oyl_schema;
    pub fn oyl_schema_builder_free(b: *mut oyl_schema_builder);
    pub fn oyl_schema_builder_add_int_flags(b: *mut oyl_schema_builder, flags: c_uint);
    pub fn oyl_schema_builder_add_float_flags(b: *mut oyl_schema_builder, flags: c_uint);
    pub fn oyl_schema_builder_add_type(b: *mut oyl_schema_builder, tag: oyl_str, parse: oyl_scalar_parser, user: *mut c_void);
    pub fn oyl_schema_value(schema: *const oyl_schema, scalar: *const oyl_event, out: *mut oyl_value) -> oyl_status;
    pub fn oyl_schema_value_of(schema: *const oyl_schema, text: oyl_str, style: oyl_scalar_style, tag: oyl_str, out: *mut oyl_value) -> oyl_status;
    pub fn oyl_schema_yaml11() -> *const oyl_schema;
    pub fn oyl_schema_goyaml2() -> *const oyl_schema;

    pub fn oyl_parser_new(input: *const c_char, len: usize, a: *mut oyl_arena) -> *mut oyl_parser;
    pub fn oyl_parse_next(p: *mut oyl_parser, evt: *mut *const oyl_event) -> oyl_status;
    pub fn oyl_parser_set_schema(p: *mut oyl_parser, schema: *const oyl_schema);
    pub fn oyl_parser_set_merge(p: *mut oyl_parser, enable: bool);
    pub fn oyl_parser_set_resolve(p: *mut oyl_parser, enable: bool);
    pub fn oyl_parser_set_max_events(p: *mut oyl_parser, max: c_int);
    pub fn oyl_parser_set_max_depth(p: *mut oyl_parser, max: c_int);
    pub fn oyl_parser_error(p: *mut oyl_parser) -> *const c_char;
    pub fn oyl_parser_error_mark(p: *mut oyl_parser) -> oyl_mark;
    pub fn oyl_parser_free(p: *mut oyl_parser);

    pub fn oyl_dialect_new(a: *mut oyl_arena, schema: *const oyl_schema, syntax: c_uint, merge: oyl_merge_order) -> *const oyl_dialect;
    pub fn oyl_dialect_schema(d: *const oyl_dialect) -> *const oyl_schema;
    pub fn oyl_dialect_syntax(d: *const oyl_dialect) -> c_uint;
    pub fn oyl_dialect_merge(d: *const oyl_dialect) -> oyl_merge_order;
    pub fn oyl_dialect_yaml12() -> *const oyl_dialect;
    pub fn oyl_dialect_yaml11() -> *const oyl_dialect;
    pub fn oyl_dialect_goyaml2() -> *const oyl_dialect;
    pub fn oyl_parser_set_dialect(p: *mut oyl_parser, d: *const oyl_dialect);
    pub fn oyl_parser_add_dialect(p: *mut oyl_parser, d: *const oyl_dialect, version: *const c_char, tag_prefix: *const c_char) -> oyl_status;
    pub fn oyl_parser_document_dialect(p: *mut oyl_parser) -> *const oyl_dialect;
    pub fn oyl_parser_yaml_version(p: *mut oyl_parser, major: *mut c_int, minor: *mut c_int) -> bool;
    pub fn oyl_parser_set_warn(p: *mut oyl_parser, fn_: oyl_warn_fn, user: *mut c_void);

    pub fn oyl_parse_json(p: *mut oyl_parser, schema: *const oyl_schema, flags: c_uint, json: *mut oyl_str) -> oyl_status;

    pub fn oyl_parse_tree(p: *mut oyl_parser, tree: *mut *mut oyl_tree) -> oyl_status;
    pub fn oyl_tree_free(t: *mut oyl_tree);
    pub fn oyl_tree_nodes(t: *const oyl_tree, count: *mut u32) -> *const oyl_node;
    pub fn oyl_tree_props(t: *const oyl_tree, count: *mut u32) -> *const oyl_node_props;
    pub fn oyl_tree_pool(t: *const oyl_tree) -> oyl_str;
    pub fn oyl_tree_base(t: *const oyl_tree) -> usize;
    pub fn oyl_tree_dialect(t: *const oyl_tree) -> *const oyl_dialect;
    pub fn oyl_node_text(t: *const oyl_tree, i: u32) -> oyl_str;
    pub fn oyl_node_tag(t: *const oyl_tree, i: u32) -> oyl_str;
    pub fn oyl_node_anchor(t: *const oyl_tree, i: u32) -> oyl_str;
    pub fn oyl_node_value(t: *const oyl_tree, i: u32, v: *mut oyl_value) -> oyl_status;
    pub fn oyl_node_mark(t: *const oyl_tree, i: u32) -> oyl_mark;
    pub fn oyl_node_target(t: *const oyl_tree, i: u32) -> u32;
    pub fn oyl_node_get(t: *const oyl_tree, map: u32, key: oyl_str) -> u32;
    pub fn oyl_node_at(t: *const oyl_tree, seq: u32, k: u32) -> u32;
    pub fn oyl_node_entries(t: *const oyl_tree, map: u32, flags: c_uint, a: *mut oyl_arena, pairs: *mut *const oyl_pair, n: *mut usize) -> oyl_status;

    pub fn oyl_emitter_new(a: *mut oyl_arena) -> *mut oyl_emitter;
    pub fn oyl_emitter_set_style(e: *mut oyl_emitter, style: oyl_emit_style);
    pub fn oyl_emitter_set_indent(e: *mut oyl_emitter, indent: c_int);
    pub fn oyl_emit(e: *mut oyl_emitter, evt: *const oyl_event) -> oyl_status;
    pub fn oyl_emit_stream_start(e: *mut oyl_emitter) -> oyl_status;
    pub fn oyl_emit_stream_end(e: *mut oyl_emitter) -> oyl_status;
    pub fn oyl_emit_document_start(e: *mut oyl_emitter, implicit: bool) -> oyl_status;
    pub fn oyl_emit_document_end(e: *mut oyl_emitter, implicit: bool) -> oyl_status;
    pub fn oyl_emit_scalar(e: *mut oyl_emitter, value: oyl_str, style: oyl_scalar_style, anchor: oyl_str, tag: oyl_str) -> oyl_status;
    pub fn oyl_emit_alias(e: *mut oyl_emitter, name: oyl_str) -> oyl_status;
    pub fn oyl_emit_mapping_start(e: *mut oyl_emitter, anchor: oyl_str, tag: oyl_str, flow: bool) -> oyl_status;
    pub fn oyl_emit_mapping_end(e: *mut oyl_emitter) -> oyl_status;
    pub fn oyl_emit_sequence_start(e: *mut oyl_emitter, anchor: oyl_str, tag: oyl_str, flow: bool) -> oyl_status;
    pub fn oyl_emit_sequence_end(e: *mut oyl_emitter) -> oyl_status;
    pub fn oyl_emit_tree(e: *mut oyl_emitter, t: *const oyl_tree) -> oyl_status;
    pub fn oyl_emitter_output(e: *mut oyl_emitter) -> oyl_str;
    pub fn oyl_emitter_free(e: *mut oyl_emitter);

    pub fn oyl_status_str(s: oyl_status) -> *const c_char;
    pub fn oyl_token_type_str(t: oyl_token_type) -> *const c_char;
    pub fn oyl_event_type_str(t: oyl_event_type) -> *const c_char;
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    extern "C" {
        static oyl_sys_layout: [usize; 48];
        static oyl_sys_layout_len: usize;
    }

    /// The Rust structs must have the C structs' sizes and field offsets.
    #[test]
    fn layout_matches_c() {
        let rust: [usize; 48] = [
            size_of::<oyl_str>(), offset_of!(oyl_str, data), offset_of!(oyl_str, len),
            size_of::<oyl_mark>(), offset_of!(oyl_mark, offset), offset_of!(oyl_mark, line),
            offset_of!(oyl_mark, col),
            size_of::<oyl_token>(), offset_of!(oyl_token, type_), offset_of!(oyl_token, value),
            offset_of!(oyl_token, scalar_style), offset_of!(oyl_token, start),
            offset_of!(oyl_token, end),
            size_of::<oyl_event>(), offset_of!(oyl_event, type_), offset_of!(oyl_event, value),
            offset_of!(oyl_event, anchor), offset_of!(oyl_event, tag),
            offset_of!(oyl_event, scalar_style), offset_of!(oyl_event, implicit),
            offset_of!(oyl_event, flow), offset_of!(oyl_event, start),
            offset_of!(oyl_event, end),
            size_of::<oyl_status>(), size_of::<oyl_event_type>(),
            size_of::<oyl_value>(), offset_of!(oyl_value, kind), offset_of!(oyl_value, as_),
            size_of::<oyl_node>(), offset_of!(oyl_node, text), offset_of!(oyl_node, len),
            offset_of!(oyl_node, span), offset_of!(oyl_node, parent), offset_of!(oyl_node, props),
            offset_of!(oyl_node, start), offset_of!(oyl_node, end), offset_of!(oyl_node, kind),
            offset_of!(oyl_node, style), offset_of!(oyl_node, type_), offset_of!(oyl_node, flags),
            size_of::<oyl_node_props>(), offset_of!(oyl_node_props, tag),
            offset_of!(oyl_node_props, tag_len), offset_of!(oyl_node_props, anchor),
            offset_of!(oyl_node_props, anchor_len),
            size_of::<oyl_pair>(), offset_of!(oyl_pair, key), offset_of!(oyl_pair, value),
        ];
        unsafe {
            assert_eq!(oyl_sys_layout_len, rust.len());
            assert_eq!(oyl_sys_layout, rust);
        }
    }

    #[test]
    fn status_names() {
        let name = unsafe { core::ffi::CStr::from_ptr(oyl_status_str(OYL_ERR_LIMIT)) };
        assert!(!name.to_bytes().is_empty());
    }
}
