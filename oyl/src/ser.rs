//! serde serialization to YAML (the `serde` feature).
//!
//! ```
//! use serde::Serialize;
//!
//! #[derive(Serialize)]
//! struct Service<'a> {
//!     name: &'a str,
//!     replicas: u32,
//!     ports: Vec<u16>,
//! }
//!
//! let yaml = oyl::to_string(&Service { name: "web", replicas: 3, ports: vec![80, 443] })?;
//! assert_eq!(yaml, "name: web\nreplicas: 3\nports:\n  - 80\n  - 443\n");
//! # Ok::<(), oyl::Error>(())
//! ```
//!
//! What it writes reads back as the same data, in YAML 1.2 and in YAML 1.1
//! and go-yaml v2 readers alike:
//!
//! - A string any of them would read as another type is quoted: `"yes"`,
//!   `"on"`, `"y"` (a bool to go-yaml v2), `"017"`, `"1:30"`, `""`.
//!   Text with line breaks is a literal block (`|`).
//! - A float always has a dot, `1.0` and `1.0e+20`; or it's `.inf`,
//!   `-.inf` or `.nan`.
//! - Bytes are base64, tagged `!!binary`.
//! - `None` and `()` are `null`.
//! - An enum's unit variant is its name; any other variant is a mapping
//!   with one key, the variant, as [`from_str`](crate::from_str) reads it.
//!
//! Integers beyond 64 bits (`i128`, `u128`) are written as numbers, but
//! Oyl reads them back as strings.

use std::fmt;
use std::io::Write;

use serde::ser::{self, Serialize};

use crate::{Emitter, Error, ErrorKind, Schema, ScalarStyle};

const STR_TAG: &[u8] = b"tag:yaml.org,2002:str";
const BINARY_TAG: &str = "tag:yaml.org,2002:binary";

impl ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error { kind: ErrorKind::Data, message: msg.to_string(), mark: None }
    }
}

/// Serialize `value` as a YAML 1.2 document, in block style.
pub fn to_string<T: ?Sized + Serialize>(value: &T) -> Result<String, Error> {
    let yaml = to_vec(value)?;
    // the emitter writes UTF-8: it is given only strs
    Ok(String::from_utf8(yaml).expect("the emitter writes UTF-8"))
}

/// As [`to_string`], as bytes.
pub fn to_vec<T: ?Sized + Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    let mut emitter = Emitter::new()?;
    emitter.stream_start()?;
    emitter.document_start(true)?;
    emitter.serialize(value)?;
    emitter.document_end(true)?;
    emitter.stream_end()?;
    Ok(emitter.output())
}

/// As [`to_string`], written to `writer`. A failed write is an
/// [`ErrorKind::Input`] error, as a failed read is in
/// [`from_reader`](crate::from_reader).
pub fn to_writer<W: Write, T: ?Sized + Serialize>(mut writer: W, value: &T) -> Result<(), Error> {
    writer
        .write_all(&to_vec(value)?)
        .map_err(|e| Error { kind: ErrorKind::Input, message: e.to_string(), mark: None })
}

impl Emitter {
    /// Write `value` as a node, where the next node goes: a document's
    /// root, a sequence's item, a key or a value. To choose the style or
    /// write several documents:
    ///
    /// ```
    /// use oyl::{EmitStyle, Emitter};
    ///
    /// let mut emitter = Emitter::new()?;
    /// emitter.style(EmitStyle::Flow).stream_start()?;
    /// for doc in [vec![1, 2], vec![3]] {
    ///     emitter.document_start(true)?;
    ///     emitter.serialize(&doc)?;
    ///     emitter.document_end(true)?;
    /// }
    /// emitter.stream_end()?;
    /// assert_eq!(emitter.output(), b"[1, 2]\n---\n[3]\n");
    /// # Ok::<(), oyl::Error>(())
    /// ```
    pub fn serialize<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut Serializer::new(self))
    }
}

/// A serde `Serializer` writing to an [`Emitter`] (see
/// [`Emitter::serialize`]).
pub struct Serializer<'e> {
    emitter: &'e mut Emitter,
}

impl<'e> Serializer<'e> {
    /// Write values as nodes with `emitter`.
    pub fn new(emitter: &'e mut Emitter) -> Self {
        Serializer { emitter }
    }

    fn plain(&mut self, text: &str) -> Result<(), Error> {
        self.emitter.scalar(text, ScalarStyle::Plain, None, None)
    }

    fn string(&mut self, s: &str) -> Result<(), Error> {
        let style = if s.contains('\n') {
            ScalarStyle::Literal
        } else if typed(s.as_bytes()) {
            ScalarStyle::DoubleQuoted
        } else {
            ScalarStyle::Plain
        };
        self.emitter.scalar(s, style, None, None)
    }

    fn float(&mut self, nan: bool, infinite: bool, negative: bool, shortest: String) -> Result<(), Error> {
        if nan {
            self.plain(".nan")
        } else if infinite {
            self.plain(if negative { "-.inf" } else { ".inf" })
        } else {
            self.plain(&yaml_float(shortest))
        }
    }

    /// Start `{variant: ...}`.
    fn variant(&mut self, variant: &str) -> Result<(), Error> {
        self.emitter.mapping_start(None, None, false)?;
        self.string(variant)
    }
}

impl fmt::Debug for Serializer<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Serializer").finish_non_exhaustive()
    }
}

/// Whether YAML 1.2, YAML 1.1 or go-yaml v2 would read `text`, written
/// plain, as anything but a string.
fn typed(text: &[u8]) -> bool {
    [Schema::core(), Schema::yaml11(), Schema::goyaml2()]
        .iter()
        .any(|schema| schema.resolve(text, ScalarStyle::Plain) != STR_TAG)
}

/// Rust's shortest float (`1.0`, `1e20`, `1e-7`) as every reader takes a
/// float: YAML 1.1 wants a dot before the exponent, and its sign.
fn yaml_float(shortest: String) -> String {
    // without an exponent, Rust writes a dot (`1.0`)
    let Some(e) = shortest.find('e') else { return shortest };
    let (mantissa, exponent) = (&shortest[..e], &shortest[e + 1..]);
    let dot = if mantissa.contains('.') { "" } else { ".0" };
    let sign = if exponent.starts_with('-') { "" } else { "+" };
    format!("{mantissa}{dot}e{sign}{exponent}")
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { char::from(BASE64[(n >> (18 - 6 * i) & 63) as usize]) } else { '=' });
        }
    }
    out
}

/// Base64 as `!!binary` holds it: line breaks and spaces are skipped.
pub(crate) fn unbase64(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut n, mut bits, mut pad) = (0u32, 0u32, 0usize);
    for &c in text {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                pad += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        if pad > 0 {
            return None;
        }
        n = n << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((n >> bits) as u8);
            n &= (1 << bits) - 1;
        }
    }
    // a lone character left over, or padding where none was due
    (bits < 6 && pad <= 2 && (pad == 0 || bits > 0)).then_some(out)
}

impl<'a, 'e> ser::Serializer for &'a mut Serializer<'e> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(self, v: bool) -> Result<(), Error> {
        self.plain(if v { "true" } else { "false" })
    }

    fn serialize_i8(self, v: i8) -> Result<(), Error> {
        self.plain(&v.to_string())
    }

    fn serialize_i16(self, v: i16) -> Result<(), Error> {
        self.plain(&v.to_string())
    }

    fn serialize_i32(self, v: i32) -> Result<(), Error> {
        self.plain(&v.to_string())
    }

    fn serialize_i64(self, v: i64) -> Result<(), Error> {
        self.plain(&v.to_string())
    }

    fn serialize_i128(self, v: i128) -> Result<(), Error> {
        self.plain(&v.to_string())
    }

    fn serialize_u8(self, v: u8) -> Result<(), Error> {
        self.plain(&v.to_string())
    }

    fn serialize_u16(self, v: u16) -> Result<(), Error> {
        self.plain(&v.to_string())
    }

    fn serialize_u32(self, v: u32) -> Result<(), Error> {
        self.plain(&v.to_string())
    }

    fn serialize_u64(self, v: u64) -> Result<(), Error> {
        self.plain(&v.to_string())
    }

    fn serialize_u128(self, v: u128) -> Result<(), Error> {
        self.plain(&v.to_string())
    }

    fn serialize_f32(self, v: f32) -> Result<(), Error> {
        self.float(v.is_nan(), v.is_infinite(), v < 0.0, format!("{v:?}"))
    }

    fn serialize_f64(self, v: f64) -> Result<(), Error> {
        self.float(v.is_nan(), v.is_infinite(), v < 0.0, format!("{v:?}"))
    }

    fn serialize_char(self, v: char) -> Result<(), Error> {
        self.string(v.encode_utf8(&mut [0; 4]))
    }

    fn serialize_str(self, v: &str) -> Result<(), Error> {
        self.string(v)
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<(), Error> {
        self.emitter.scalar(base64(v), ScalarStyle::Plain, None, Some(BINARY_TAG))
    }

    fn serialize_none(self) -> Result<(), Error> {
        self.plain("null")
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<(), Error> {
        self.plain("null")
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Error> {
        self.plain("null")
    }

    fn serialize_unit_variant(self, _name: &'static str, _index: u32, variant: &'static str) -> Result<(), Error> {
        self.string(variant)
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _name: &'static str, value: &T) -> Result<(), Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.variant(variant)?;
        value.serialize(&mut *self)?;
        self.emitter.mapping_end()
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self, Error> {
        self.emitter.sequence_start(None, None, false)?;
        Ok(self)
    }

    fn serialize_tuple(self, len: usize) -> Result<Self, Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<Self, Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(self, _name: &'static str, _index: u32, variant: &'static str, len: usize) -> Result<Self, Error> {
        self.variant(variant)?;
        self.serialize_seq(Some(len))
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self, Error> {
        self.emitter.mapping_start(None, None, false)?;
        Ok(self)
    }

    fn serialize_struct(self, _name: &'static str, len: usize) -> Result<Self, Error> {
        self.serialize_map(Some(len))
    }

    fn serialize_struct_variant(self, _name: &'static str, _index: u32, variant: &'static str, len: usize) -> Result<Self, Error> {
        self.variant(variant)?;
        self.serialize_map(Some(len))
    }
}

impl ser::SerializeSeq for &mut Serializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Error> {
        self.emitter.sequence_end()
    }
}

impl ser::SerializeTuple for &mut Serializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Error> {
        self.emitter.sequence_end()
    }
}

impl ser::SerializeTupleStruct for &mut Serializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Error> {
        self.emitter.sequence_end()
    }
}

impl ser::SerializeTupleVariant for &mut Serializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Error> {
        self.emitter.sequence_end()?;
        self.emitter.mapping_end()
    }
}

impl ser::SerializeMap for &mut Serializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), Error> {
        key.serialize(&mut **self)
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Error> {
        self.emitter.mapping_end()
    }
}

impl ser::SerializeStruct for &mut Serializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, key: &'static str, value: &T) -> Result<(), Error> {
        self.string(key)?;
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Error> {
        self.emitter.mapping_end()
    }
}

impl ser::SerializeStructVariant for &mut Serializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, key: &'static str, value: &T) -> Result<(), Error> {
        self.string(key)?;
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Error> {
        self.emitter.mapping_end()?;
        self.emitter.mapping_end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_every_reader_takes() {
        assert_eq!(yaml_float("1.5".into()), "1.5");
        assert_eq!(yaml_float("1e20".into()), "1.0e+20");
        assert_eq!(yaml_float("-2.5e-7".into()), "-2.5e-7");
    }

    #[test]
    fn base64_both_ways() {
        for data in [&b""[..], b"h", b"hi", b"hi!", b"\x00\xff\x10\x80"] {
            assert_eq!(unbase64(base64(data).as_bytes()).as_deref(), Some(data));
        }
        assert_eq!(base64(b"hi"), "aGk=");
        assert_eq!(unbase64(b"aG\n k=").as_deref(), Some(&b"hi"[..]));
        for bad in [&b"a"[..], b"aGk=a", b"aGk===", b"a*k=", b"aGVs="] {
            assert_eq!(unbase64(bad), None, "{:?}", std::str::from_utf8(bad));
        }
    }
}
