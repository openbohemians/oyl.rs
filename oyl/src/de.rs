//! serde deserialization from a document's tree (the `serde` feature).
//!
//! ```
//! use serde::Deserialize;
//!
//! #[derive(Deserialize)]
//! struct Service<'a> {
//!     name: &'a str,
//!     replicas: u32,
//!     ports: Vec<u16>,
//! }
//!
//! let s: Service = oyl::from_str("name: web\nreplicas: 3\nports: [80, 443]\n")?;
//! assert_eq!((s.name, s.replicas, s.ports), ("web", 3, vec![80, 443]));
//! # Ok::<(), oyl::Error>(())
//! ```
//!
//! The YAML is read into a [`Tree`] and deserialized from it:
//!
//! - Scalars are typed by the dialect: YAML 1.2's by default, or the one
//!   [`from_str_with`] is given. A field wanting a string takes any
//!   scalar's text (`version: 1.20` is `"1.20"`), except a null.
//! - `&str` fields borrow from the input, unless the parser rewrote the
//!   text (escapes, folded lines): use `String` or `Cow<str>` for those.
//! - Aliases are followed, and `<<` merge keys applied in the dialect's
//!   order. A key written twice in a mapping is an error.
//! - An enum is a string (a unit variant), a mapping with one key (the
//!   variant and its content), or a node tagged `!Variant`.
//! - Bytes are a `!!binary` scalar's base64, decoded, or any other
//!   scalar's text.
//! - Errors say where: the line and column of the node that didn't fit.
//!
//! Nesting deeper than 128 is an error, and so is expanding aliases far
//! beyond the document's size, so cyclic aliases and alias bombs fail
//! instead of exhausting the stack or memory.

use std::cell::Cell;
use std::fmt;
use std::io::Read;
use std::ptr::NonNull;

use serde::de::{self, DeserializeOwned, DeserializeSeed, IntoDeserializer, Visitor};
use serde::Deserialize;

use oyl_sys as sys;

use crate::{Dialect, Error, ErrorKind, Items, Node, NodeKind, Parser, Tree, Value};

/// How deep collections may nest, aliases followed.
const MAX_DEPTH: u32 = 128;

const BINARY_TAG: &[u8] = b"tag:yaml.org,2002:binary";

impl de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error { kind: ErrorKind::Data, message: msg.to_string(), mark: None }
    }
}

/// Deserialize a `T` from YAML: one document, read as YAML 1.2. No document
/// at all reads as null.
pub fn from_str<'de, T: Deserialize<'de>>(input: &'de str) -> Result<T, Error> {
    from_slice(input.as_bytes())
}

/// As [`from_str`], reading by `dialect`: with [`Dialect::goyaml2`], as
/// Kubernetes reads YAML (`enabled: yes` is a bool, `mode: 0644` octal).
pub fn from_str_with<'de, T: Deserialize<'de>>(input: &'de str, dialect: Dialect<'_>) -> Result<T, Error> {
    from_slice_with(input.as_bytes(), dialect)
}

/// As [`from_str`], from bytes.
pub fn from_slice<'de, T: Deserialize<'de>>(input: &'de [u8]) -> Result<T, Error> {
    from_slice_with(input, Dialect::yaml12())
}

/// As [`from_str_with`], from bytes.
pub fn from_slice_with<'de, T: Deserialize<'de>>(input: &'de [u8], dialect: Dialect<'_>) -> Result<T, Error> {
    let mut parser = Parser::new(input)?;
    // a tree keeps aliases, so its events don't grow beyond the input
    parser.dialect(dialect).max_events(0);
    let Some(tree) = parser.next_tree()? else {
        return T::deserialize(de::value::UnitDeserializer::<Error>::new());
    };
    if let Some(next) = parser.next_tree()? {
        return Err(Error {
            kind: ErrorKind::Data,
            message: "more than one document: read them with Parser::trees and from_node".into(),
            mark: Some(next.root().mark()),
        });
    }
    let state = State::new(&tree)?;
    T::deserialize(De { node: tree.root(), src: InInput(input), state: &state, skip_tag: false })
}

/// As [`from_slice`], reading all of `reader` first.
pub fn from_reader<R: Read, T: DeserializeOwned>(mut reader: R) -> Result<T, Error> {
    let mut input = Vec::new();
    reader
        .read_to_end(&mut input)
        .map_err(|e| Error { kind: ErrorKind::Input, message: e.to_string(), mark: None })?;
    from_slice(&input)
}

/// Deserialize a `T` from a node of a tree you hold: a document read with
/// [`Parser::trees`], or part of one. Strings borrow from the tree.
///
/// ```
/// use oyl::Parser;
///
/// let parser = Parser::new("--- [1, 2]\n--- [3]\n")?;
/// let mut all = Vec::new();
/// for tree in parser.trees() {
///     all.extend(oyl::from_node::<Vec<i32>>(tree?.root())?);
/// }
/// assert_eq!(all, [1, 2, 3]);
/// # Ok::<(), oyl::Error>(())
/// ```
pub fn from_node<'t, T: Deserialize<'t>>(node: Node<'t>) -> Result<T, Error> {
    let state = State::new(node.tree())?;
    T::deserialize(De { node, src: InTree(node.tree()), state: &state, skip_tag: false })
}

// ── Where borrowed text comes from ──────────────────────────

/// Gives a scalar's text for the lifetime `'de`, when it can.
trait Source<'de>: Copy {
    fn borrow(&self, node: Node<'_>) -> Option<&'de [u8]>;
}

/// Deserializing from the input: text the parser didn't rewrite is a
/// slice of it.
#[derive(Clone, Copy)]
struct InInput<'de>(&'de [u8]);

impl<'de> Source<'de> for InInput<'de> {
    fn borrow(&self, node: Node<'_>) -> Option<&'de [u8]> {
        let text = node.text();
        if text.is_empty() {
            return Some(&[]);
        }
        let start = (text.as_ptr() as usize).checked_sub(self.0.as_ptr() as usize)?;
        self.0.get(start..start.checked_add(text.len())?).filter(|s| s.as_ptr() == text.as_ptr())
    }
}

/// Deserializing from a tree the caller holds: all its text lives as long.
#[derive(Clone, Copy)]
struct InTree<'de>(&'de Tree<'de>);

impl<'de> Source<'de> for InTree<'de> {
    fn borrow(&self, node: Node<'_>) -> Option<&'de [u8]> {
        self.0.node(node.index()).map(|n| n.text())
    }
}

// ── Limits ──────────────────────────────────────────────────

struct State {
    /// Nodes left to visit: aliases can make a document's nodes visited
    /// many times over.
    budget: Cell<usize>,
    depth: Cell<u32>,
    /// Reused for each mapping's entries.
    arena: NonNull<sys::oyl_arena>,
}

impl State {
    fn new(tree: &Tree<'_>) -> Result<Self, Error> {
        let arena = NonNull::new(unsafe { sys::oyl_arena_new(4096) }).ok_or_else(Error::memory)?;
        let budget = tree.node_count().saturating_mul(100).saturating_add(1_000_000);
        Ok(State { budget: Cell::new(budget), depth: Cell::new(0), arena })
    }

    fn visit(&self) -> Result<(), Error> {
        match self.budget.get().checked_sub(1) {
            Some(left) => {
                self.budget.set(left);
                Ok(())
            }
            None => Err(Error { kind: ErrorKind::Limit, message: "aliases expand too far".into(), mark: None }),
        }
    }

    fn enter(&self) -> Result<Depth<'_>, Error> {
        let d = self.depth.get() + 1;
        if d > MAX_DEPTH {
            return Err(Error { kind: ErrorKind::Limit, message: format!("nested more than {MAX_DEPTH} deep"), mark: None });
        }
        self.depth.set(d);
        Ok(Depth(self))
    }
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe { sys::oyl_arena_free(self.arena.as_ptr()) };
    }
}

/// One level of nesting, left when dropped.
struct Depth<'s>(&'s State);

impl Drop for Depth<'_> {
    fn drop(&mut self) {
        self.0.depth.set(self.0.depth.get() - 1);
    }
}

// ── The deserializer ────────────────────────────────────────

struct De<'s, S> {
    node: Node<'s>,
    src: S,
    state: &'s State,
    /// The node's `!Variant` tag has named an enum variant already.
    skip_tag: bool,
}

impl<'s, S: Copy> Clone for De<'s, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'s, S: Copy> Copy for De<'s, S> {}

fn utf8(b: &[u8]) -> Result<&str, Error> {
    std::str::from_utf8(b).map_err(|_| de::Error::custom("text is not UTF-8"))
}

impl<'s, 'de, S: Source<'de>> De<'s, S> {
    fn child(&self, node: Node<'s>) -> Result<Self, Error> {
        self.state.visit()?;
        Ok(De { node, src: self.src, state: self.state, skip_tag: false })
    }

    /// Through an alias, to the node it refers to.
    fn target(self) -> Result<Self, Error> {
        if self.node.kind() == NodeKind::Alias { self.child(self.node.target()) } else { Ok(self) }
    }

    /// Give an error the position of this node, unless it has one.
    fn at(&self, mut e: Error) -> Error {
        if e.mark.is_none() {
            e.mark = Some(self.node.mark());
        }
        e
    }

    /// A scalar's value; with its `!Variant` tag spent, as if untagged.
    fn value(&self) -> Result<Value<'s>, Error> {
        if self.skip_tag {
            let schema = self.node.tree().dialect().schema();
            return schema.value(self.node.text(), self.node.style(), None).map_err(|e| self.at(e));
        }
        self.node.value()
    }

    fn is_null(&self) -> Result<bool, Error> {
        Ok(self.node.kind() == NodeKind::Scalar && self.value()?.is_null())
    }

    fn text<V: Visitor<'de>>(&self, visitor: V) -> Result<V::Value, Error> {
        match self.src.borrow(self.node) {
            Some(b) => visitor.visit_borrowed_str(utf8(b)?),
            None => visitor.visit_str(utf8(self.node.text())?),
        }
    }

    fn any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.node.kind() {
            NodeKind::Sequence => self.seq(visitor),
            NodeKind::Mapping => self.map(visitor),
            NodeKind::Alias => self.target()?.any(visitor),
            NodeKind::Scalar => match self.value()? {
                Value::Null => visitor.visit_unit(),
                Value::Bool(b) => visitor.visit_bool(b),
                Value::Int(i) => visitor.visit_i64(i),
                Value::Uint(u) => visitor.visit_u64(u),
                Value::Float(f) => visitor.visit_f64(f),
                Value::Str(_) => self.text(visitor),
            },
        }
    }

    fn seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let _depth = self.state.enter()?;
        let mut access = Seq { items: self.node.items(), de: self };
        let value = visitor.visit_seq(&mut access)?;
        match access.items.len() {
            0 => Ok(value),
            left => Err(de::Error::invalid_length(self.node.len(), &format!("{} fewer items", left).as_str())),
        }
    }

    fn map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let _depth = self.state.enter()?;
        let entries = self.node.entries_in(self.state.arena, sys::OYL_ENTRIES_STRICT);
        unsafe { sys::oyl_arena_reset(self.state.arena.as_ptr()) };
        let mut access = Map { entries: entries?.into_iter(), value: None, de: self };
        let value = visitor.visit_map(&mut access)?;
        match access.entries.len() {
            0 => Ok(value),
            left => Err(de::Error::custom(format!("{left} keys left over"))),
        }
    }

    /// A `!Variant` tag written on the node.
    fn variant_tag(&self) -> Option<&'s [u8]> {
        if self.skip_tag {
            return None;
        }
        let tag = self.node.tag()?;
        tag.strip_prefix(b"!").filter(|name| !name.is_empty())
    }
}

macro_rules! forward_to_any {
    ($($method:ident)*) => {$(
        fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
            self.deserialize_any(visitor)
        }
    )*};
}

impl<'s, 'de, S: Source<'de>> de::Deserializer<'de> for De<'s, S> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.any(visitor).map_err(|e| self.at(e))
    }

    forward_to_any! {
        deserialize_bool deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64 deserialize_i128
        deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64 deserialize_u128
        deserialize_f32 deserialize_f64
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let de = self.target()?;
        let out = match de.node.kind() {
            // any scalar's text, but a null isn't a string
            NodeKind::Scalar if !de.is_null()? => de.text(visitor),
            _ => de.any(visitor),
        };
        out.map_err(|e| de.at(e))
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_str(visitor)
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_str(visitor)
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let de = self.target()?;
        let out = match de.node.kind() {
            NodeKind::Scalar if de.node.tag() == Some(BINARY_TAG) => match crate::ser::unbase64(de.node.text()) {
                Some(b) => visitor.visit_byte_buf(b),
                None => Err(de::Error::custom("invalid base64 in !!binary")),
            },
            NodeKind::Scalar => match de.src.borrow(de.node) {
                Some(b) => visitor.visit_borrowed_bytes(b),
                None => visitor.visit_bytes(de.node.text()),
            },
            _ => de.any(visitor),
        };
        out.map_err(|e| de.at(e))
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let de = self.target()?;
        let out = if de.is_null()? { visitor.visit_none() } else { visitor.visit_some(de) };
        out.map_err(|e| de.at(e))
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_any(visitor)
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(self, _name: &'static str, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_any(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _name: &'static str, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_any(visitor)
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_any(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(self, _name: &'static str, _len: usize, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_any(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_any(visitor)
    }

    fn deserialize_struct<V: Visitor<'de>>(self, _name: &'static str, _fields: &'static [&'static str], visitor: V) -> Result<V::Value, Error> {
        self.deserialize_any(visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(self, _name: &'static str, _variants: &'static [&'static str], visitor: V) -> Result<V::Value, Error> {
        let de = self.target()?;
        let out = if let Some(name) = de.variant_tag() {
            visitor.visit_enum(Enum { variant: VariantName::Tag(name), content: De { skip_tag: true, ..de } })
        } else {
            match de.node.kind() {
                NodeKind::Scalar => visitor.visit_enum(utf8(de.node.text())?.into_deserializer()),
                NodeKind::Mapping if de.node.len() == 1 => {
                    let (key, value) = de.node.pairs().next().expect("a mapping with one pair");
                    visitor.visit_enum(Enum { variant: VariantName::Key(de.child(key)?), content: de.child(value)? })
                }
                _ => Err(de::Error::custom("expected an enum: a string, a mapping with one key, or a !Variant tag")),
            }
        };
        out.map_err(|e| de.at(e))
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_unit()
    }
}

// ── Sequences and mappings ──────────────────────────────────

struct Seq<'s, S> {
    items: Items<'s>,
    de: De<'s, S>,
}

impl<'s, 'de, S: Source<'de>> de::SeqAccess<'de> for Seq<'s, S> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>, Error> {
        match self.items.next() {
            Some(item) => seed.deserialize(self.de.child(item)?).map(Some),
            None => Ok(None),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.items.len())
    }
}

struct Map<'s, S> {
    entries: std::vec::IntoIter<(Node<'s>, Node<'s>)>,
    value: Option<Node<'s>>,
    de: De<'s, S>,
}

impl<'s, 'de, S: Source<'de>> de::MapAccess<'de> for Map<'s, S> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>, Error> {
        match self.entries.next() {
            Some((key, value)) => {
                self.value = Some(value);
                seed.deserialize(self.de.child(key)?).map(Some)
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let value = self.value.take().ok_or_else(|| de::Error::custom("a value asked for before its key"))?;
        seed.deserialize(self.de.child(value)?)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.entries.len())
    }
}

// ── Enums ───────────────────────────────────────────────────

enum VariantName<'s, S> {
    /// From a `!Variant` tag.
    Tag(&'s [u8]),
    /// The key of a mapping with one pair.
    Key(De<'s, S>),
}

struct Enum<'s, S> {
    variant: VariantName<'s, S>,
    content: De<'s, S>,
}

impl<'s, 'de, S: Source<'de>> de::EnumAccess<'de> for Enum<'s, S> {
    type Error = Error;
    type Variant = De<'s, S>;

    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, De<'s, S>), Error> {
        let variant = match self.variant {
            VariantName::Tag(name) => seed.deserialize(utf8(name)?.into_deserializer())?,
            VariantName::Key(key) => seed.deserialize(key)?,
        };
        Ok((variant, self.content))
    }
}

impl<'s, 'de, S: Source<'de>> de::VariantAccess<'de> for De<'s, S> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        let de = self.target()?;
        if de.is_null()? {
            Ok(())
        } else {
            Err(de.at(de::Error::custom("a unit variant takes no value")))
        }
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(self)
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_seq(self, visitor)
    }

    fn struct_variant<V: Visitor<'de>>(self, _fields: &'static [&'static str], visitor: V) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_map(self, visitor)
    }
}
