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
//! one (fields may be appended in later versions).

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
opaque!(oyl_arena, oyl_scanner, oyl_parser, oyl_emitter, oyl_schema, oyl_schema_builder);

pub type oyl_match_type = c_uint;
pub const OYL_MATCH_EXACT: oyl_match_type = 0;
pub const OYL_MATCH_ICASE: oyl_match_type = 1;
pub const OYL_MATCH_BUILTIN: oyl_match_type = 2;

pub type oyl_emit_style = c_uint;
pub const OYL_EMIT_BLOCK: oyl_emit_style = 0;
pub const OYL_EMIT_FLOW: oyl_emit_style = 1;
pub const OYL_EMIT_MINIMAL: oyl_emit_style = 2;

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
        static oyl_sys_layout: [usize; 25];
        static oyl_sys_layout_len: usize;
    }

    /// The Rust structs must have the C structs' sizes and field offsets.
    #[test]
    fn layout_matches_c() {
        let rust: [usize; 25] = [
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
