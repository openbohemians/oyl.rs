#![cfg(feature = "serde")]

use std::collections::BTreeMap;
use std::fmt;

use oyl::{Dialect, EmitStyle, Emitter, ErrorKind};
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Config {
    name: String,
    port: u16,
    debug: bool,
    ratio: f64,
    tags: Vec<String>,
    owner: Option<String>,
    limits: BTreeMap<String, u32>,
}

#[test]
fn a_struct() {
    let c = Config {
        name: "web".into(),
        port: 8080,
        debug: false,
        ratio: 0.5,
        tags: vec!["a".into(), "on".into()],
        owner: None,
        limits: [("cpu".to_string(), 2), ("mem".to_string(), 512)].into(),
    };
    let yaml = oyl::to_string(&c).unwrap();
    assert_eq!(
        yaml,
        "name: web\nport: 8080\ndebug: false\nratio: 0.5\ntags:\n  - a\n  - \"on\"\nowner: null\nlimits:\n  cpu: 2\n  mem: 512\n"
    );
    assert_eq!(oyl::from_str::<Config>(&yaml).unwrap(), c);
}

#[test]
fn strings_other_readers_would_type_are_quoted() {
    for s in ["true", "null", "~", "42", "0x1F", "1.5", ".inf", "yes", "No", "on", "OFF", "y", "n", "017", "1:30", "2001-12-14", "<<", "=", ""] {
        assert_eq!(oyl::to_string(s).unwrap(), format!("\"{s}\"\n"));
    }
    assert_eq!(oyl::to_string("hello").unwrap(), "hello\n");
    assert_eq!(oyl::to_string(&'y').unwrap(), "\"y\"\n");
    assert_eq!(oyl::to_string("one\ntwo\n").unwrap(), "|\n  one\n  two\n");
}

#[test]
fn numbers() {
    assert_eq!(oyl::to_string(&[1.0, 1e20, -2.5e-7, 0.1]).unwrap(), "- 1.0\n- 1.0e+20\n- -2.5e-7\n- 0.1\n");
    assert_eq!(oyl::to_string(&[f64::INFINITY, f64::NEG_INFINITY, f64::NAN]).unwrap(), "- .inf\n- -.inf\n- .nan\n");
    assert_eq!(oyl::to_string(&1.5f32).unwrap(), "1.5\n");
    assert_eq!(oyl::to_string(&u64::MAX).unwrap(), "18446744073709551615\n");
    assert_eq!(oyl::to_string(&i64::MIN).unwrap(), "-9223372036854775808\n");
    assert!(oyl::from_str::<f64>(".nan").unwrap().is_nan());
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Shape {
    Dot,
    Circle(f64),
    Rect(u32, u32),
    Named { name: String },
}

#[test]
fn enums() {
    let shapes = vec![Shape::Dot, Shape::Circle(1.0), Shape::Rect(2, 3), Shape::Named { name: "yes".into() }];
    let yaml = oyl::to_string(&shapes).unwrap();
    assert_eq!(yaml, "- Dot\n- Circle: 1.0\n- Rect:\n    - 2\n    - 3\n- Named:\n    name: \"yes\"\n");
    assert_eq!(oyl::from_str::<Vec<Shape>>(&yaml).unwrap(), shapes);
}

/// Bytes, (de)serialized as bytes, not a sequence.
#[derive(Debug, PartialEq)]
struct Bytes(Vec<u8>);

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Bytes;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("bytes")
            }
            fn visit_bytes<E: de::Error>(self, b: &[u8]) -> Result<Bytes, E> {
                Ok(Bytes(b.to_vec()))
            }
            fn visit_byte_buf<E: de::Error>(self, b: Vec<u8>) -> Result<Bytes, E> {
                Ok(Bytes(b))
            }
        }
        d.deserialize_byte_buf(V)
    }
}

#[test]
fn bytes_are_binary() {
    assert_eq!(oyl::to_string(&Bytes(b"hi".to_vec())).unwrap(), "!!binary aGk=\n");
    for data in [&b""[..], b"\x00\xff", b"hello, world"] {
        let yaml = oyl::to_string(&Bytes(data.to_vec())).unwrap();
        assert_eq!(oyl::from_str::<Bytes>(&yaml).unwrap().0, data, "{yaml:?}");
    }
    // as written by others: broken over lines
    assert_eq!(oyl::from_str::<Bytes>("!!binary |\n  aGVs\n  bG8=\n").unwrap().0, b"hello");
    // an untagged scalar is its text
    assert_eq!(oyl::from_str::<Bytes>("aGk=").unwrap().0, b"aGk=");
    let err = oyl::from_str::<Bytes>("!!binary a*").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Data);
}

#[test]
fn collections_and_keys() {
    let pairs: BTreeMap<(i32, bool), Option<()>> = [((1, true), None), ((2, false), Some(()))].into();
    assert_eq!(
        oyl::to_string(&pairs).unwrap(),
        "?\n  - 1\n  - true\n: null\n?\n  - 2\n  - false\n: null\n"
    );
    let ints: BTreeMap<i64, &str> = [(1, "one"), (-2, "two")].into();
    let mut e = Emitter::new().unwrap();
    e.style(EmitStyle::Flow).stream_start().unwrap();
    e.document_start(true).unwrap();
    e.serialize(&ints).unwrap();
    e.document_end(true).unwrap();
    e.stream_end().unwrap();
    assert_eq!(e.output(), b"{-2: two, 1: one}\n");
}

#[test]
fn to_writer_and_to_vec() {
    let mut out = Vec::new();
    oyl::to_writer(&mut out, &vec!["a", "b"]).unwrap();
    assert_eq!(out, b"- a\n- b\n");
    assert_eq!(oyl::to_vec(&()).unwrap(), b"null\n");
}

#[test]
fn a_failing_serialize_is_a_data_error() {
    struct Fails;
    impl Serialize for Fails {
        fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("no"))
        }
    }
    let err = oyl::to_string(&Fails).unwrap_err();
    assert_eq!((err.kind(), err.message()), (ErrorKind::Data, "no"));
}

// ── Random values read back the same, by every dialect ──────

/// Any value, as YAML types it: read with `deserialize_any`, so a string
/// written plain and read as a bool reads back different.
#[derive(Debug, Clone, PartialEq)]
enum Gen {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Str(String),
    Seq(Vec<Gen>),
    Map(Vec<(Gen, Gen)>),
}

impl Serialize for Gen {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Gen::Null => s.serialize_unit(),
            Gen::Bool(b) => s.serialize_bool(*b),
            Gen::Int(i) => s.serialize_i64(*i),
            Gen::Uint(u) => s.serialize_u64(*u),
            Gen::Float(f) => s.serialize_f64(*f),
            Gen::Str(t) => s.serialize_str(t),
            Gen::Seq(items) => items.serialize(s),
            Gen::Map(pairs) => {
                let mut m = s.serialize_map(Some(pairs.len()))?;
                for (k, v) in pairs {
                    m.serialize_entry(k, v)?;
                }
                m.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Gen {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Gen;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_unit<E>(self) -> Result<Gen, E> {
                Ok(Gen::Null)
            }
            fn visit_bool<E>(self, b: bool) -> Result<Gen, E> {
                Ok(Gen::Bool(b))
            }
            fn visit_i64<E>(self, i: i64) -> Result<Gen, E> {
                Ok(Gen::Int(i))
            }
            fn visit_u64<E>(self, u: u64) -> Result<Gen, E> {
                Ok(Gen::Uint(u))
            }
            fn visit_f64<E>(self, f: f64) -> Result<Gen, E> {
                Ok(Gen::Float(f))
            }
            fn visit_str<E>(self, s: &str) -> Result<Gen, E> {
                Ok(Gen::Str(s.to_owned()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Gen, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = a.next_element()? {
                    items.push(item);
                }
                Ok(Gen::Seq(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Gen, A::Error> {
                let mut pairs = Vec::new();
                while let Some(pair) = a.next_entry()? {
                    pairs.push(pair);
                }
                Ok(Gen::Map(pairs))
            }
        }
        d.deserialize_any(V)
    }
}

/// xorshift64: enough for test values, with no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const PIECES: &[&str] = &[
    "a", "b", "yes", "no", "on", "y", "N", "true", "null", "~", "0", "017", "0x1F", "0o7", "0b101", "1:30", "1_000",
    "2001-12-14", ".5", "-.5", "1e3", ".inf", "-", ":", "#", " ", "  ", "\t", "\n", "\r", "\r\n", "'", "\"", "\\", "&a",
    "*a", "!x", "|", ">", "%", "@", "`", "{", "}", "[", "]", ",", "?", "é", "日本", "\u{85}", "\u{2028}", "\u{0}",
    "\u{7F}", "\u{FEFF}", "<<", "=", "- x", ": y", "? z", "...", "---",
];

fn string(r: &mut Rng) -> String {
    (0..r.below(6)).map(|_| PIECES[r.below(PIECES.len() as u64) as usize]).collect()
}

fn scalar(r: &mut Rng) -> Gen {
    match r.below(8) {
        0 => Gen::Null,
        1 => Gen::Bool(r.below(2) == 1),
        2 => Gen::Int(r.next() as i64),
        3 => Gen::Uint(r.next() | 1 << 63),
        4 => Gen::Float(Some(f64::from_bits(r.next())).filter(|f| !f.is_nan()).unwrap_or(0.25)),
        5 => Gen::Float([f64::INFINITY, f64::NEG_INFINITY, 0.0, 1.0, 1e300, 5e-324, 1e-7][r.below(7) as usize]),
        _ => Gen::Str(string(r)),
    }
}

fn value(r: &mut Rng, depth: u32) -> Gen {
    if depth >= 3 || r.below(3) > 0 {
        return scalar(r);
    }
    if r.below(2) == 0 {
        Gen::Seq((0..r.below(5)).map(|_| value(r, depth + 1)).collect())
    } else {
        // string keys, each once: a key written twice is an error
        let mut pairs: Vec<(Gen, Gen)> = Vec::new();
        for _ in 0..r.below(5) {
            let key = Gen::Str(string(r));
            if !pairs.iter().any(|(k, _)| *k == key) {
                pairs.push((key, value(r, depth + 1)));
            }
        }
        Gen::Map(pairs)
    }
}

#[test]
fn random_values_read_back_the_same() {
    let mut r = Rng(42);
    for style in [EmitStyle::Block, EmitStyle::Flow, EmitStyle::Minimal] {
        for _ in 0..2000 {
            let v = value(&mut r, 0);
            let mut e = Emitter::new().unwrap();
            e.style(style).indent(1 + r.below(10) as u8);
            e.stream_start().unwrap();
            e.document_start(true).unwrap();
            e.serialize(&v).unwrap();
            e.document_end(true).unwrap();
            e.stream_end().unwrap();
            let yaml = String::from_utf8(e.output()).unwrap();
            for (name, dialect) in [("yaml12", Dialect::yaml12()), ("yaml11", Dialect::yaml11()), ("goyaml2", Dialect::goyaml2())] {
                let back: Gen = oyl::from_str_with(&yaml, dialect).unwrap_or_else(|e| panic!("{v:?} wrote {yaml:?}, which {name} can't read: {e}"));
                assert_eq!(back, v, "{v:?} wrote {yaml:?}, read by {name}");
            }
        }
    }
}
