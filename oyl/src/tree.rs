// Trees: a document in memory (oyl_parse_tree), read in place.

use std::fmt;
use std::marker::PhantomData;
use std::ops::Range;
use std::ptr::NonNull;
use std::str::Utf8Error;

use oyl_sys as sys;

use crate::{bytes, opt_bytes, raw_str, Dialect, Error, ErrorKind, Mark, Parser, ScalarStyle, State, Value};

/// A document in memory, read from a [`Parser`] with
/// [`next_tree`](Parser::next_tree) or [`trees`](Parser::trees).
///
/// Its nodes are in document order, the root first. Text stays in the
/// input, except what the parser rewrote (escapes, folded lines), which the
/// tree keeps; so a tree borrows the input but not the parser, and can
/// outlive it. Aliases and `<<` merge keys stay as written:
/// [`Node::get`] follows aliases and applies merges in the order of the
/// document's [`Dialect`]. A tree is read-only; threads may share it.
pub struct Tree<'i> {
    raw: NonNull<sys::oyl_tree>,
    nodes: *const sys::oyl_node,
    count: u32,
    _input: PhantomData<&'i [u8]>,
}

// SAFETY: a finished tree is never changed, and reading it changes nothing
unsafe impl Send for Tree<'_> {}
unsafe impl Sync for Tree<'_> {}

impl<'i> Tree<'i> {
    /// The root node.
    pub fn root(&self) -> Node<'_> {
        Node { tree: self.shorten(), i: 0 }
    }

    /// How many nodes the tree holds (at least one: an empty document is
    /// an empty scalar).
    pub fn node_count(&self) -> usize {
        self.count as usize
    }

    /// Node `index` in document order.
    pub fn node(&self, index: usize) -> Option<Node<'_>> {
        let i = u32::try_from(index).ok().filter(|&i| i < self.count)?;
        Some(Node { tree: self.shorten(), i })
    }

    /// Every node, in document order.
    pub fn nodes(&self) -> impl ExactSizeIterator<Item = Node<'_>> + '_ {
        let tree = self.shorten();
        (0..self.count).map(move |i| Node { tree, i })
    }

    /// The node array as Oyl lays it out (oyl.h, "Trees"), for walking
    /// without calls: a collection's first child is the next node, and the
    /// next sibling of node `i` is node `i + span`.
    pub fn raw_nodes(&self) -> &[sys::oyl_node] {
        // SAFETY: the array has `count` nodes and lives as long as the tree
        unsafe { std::slice::from_raw_parts(self.nodes, self.count as usize) }
    }

    /// The document's dialect, which typed its scalars.
    pub fn dialect(&self) -> Dialect<'_> {
        let d = unsafe { sys::oyl_tree_dialect(self.raw()) };
        Dialect(NonNull::new(d.cast_mut()).expect("a tree has a dialect"), PhantomData)
    }

    /// Where the document starts in the input.
    pub fn base(&self) -> usize {
        unsafe { sys::oyl_tree_base(self.raw()) }
    }

    pub(crate) fn raw(&self) -> *const sys::oyl_tree {
        self.raw.as_ptr()
    }

    fn shorten(&self) -> &Tree<'_> {
        self
    }

    fn raw_node(&self, i: u32) -> &sys::oyl_node {
        &self.raw_nodes()[i as usize]
    }
}

impl Drop for Tree<'_> {
    fn drop(&mut self) {
        unsafe { sys::oyl_tree_free(self.raw.as_ptr()) };
    }
}

impl fmt::Debug for Tree<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tree").field("nodes", &self.count).field("dialect", &self.dialect()).finish()
    }
}

/// What a [`Node`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// A scalar: [`Node::text`] and [`Node::value`].
    Scalar,
    /// A sequence: [`Node::items`].
    Sequence,
    /// A mapping: [`Node::pairs`], [`Node::get`], [`Node::entries`].
    Mapping,
    /// An alias (`*name`): [`Node::target`] is the node it refers to.
    Alias,
}

/// A node of a [`Tree`]: a cheap handle (the tree and an index) whose text
/// borrows from the tree.
///
/// The collection methods ([`get`](Node::get), [`at`](Node::at),
/// [`items`](Node::items), [`pairs`](Node::pairs), [`len`](Node::len),
/// [`entries`](Node::entries)) follow an alias to its target.
#[derive(Clone, Copy)]
pub struct Node<'t> {
    tree: &'t Tree<'t>,
    i: u32,
}

impl<'t> Node<'t> {
    /// Its index in the tree's document order.
    pub fn index(&self) -> usize {
        self.i as usize
    }

    /// The tree it belongs to.
    pub fn tree(&self) -> &'t Tree<'t> {
        self.tree
    }

    /// The node as Oyl lays it out.
    pub fn raw(&self) -> &'t sys::oyl_node {
        self.tree.raw_node(self.i)
    }

    /// What the node is.
    pub fn kind(&self) -> NodeKind {
        match self.raw().kind as sys::oyl_node_kind {
            sys::OYL_NODE_SEQUENCE => NodeKind::Sequence,
            sys::OYL_NODE_MAPPING => NodeKind::Mapping,
            sys::OYL_NODE_ALIAS => NodeKind::Alias,
            _ => NodeKind::Scalar,
        }
    }

    /// A scalar's text, or an alias's anchor name; empty for a collection.
    pub fn text(&self) -> &'t [u8] {
        // SAFETY: the text is in the input or the tree's pool, which outlive 't
        unsafe { bytes(sys::oyl_node_text(self.tree.raw(), self.i)) }
    }

    /// The text as UTF-8 (input that is valid UTF-8 gives valid UTF-8).
    pub fn text_str(&self) -> Result<&'t str, Utf8Error> {
        std::str::from_utf8(self.text())
    }

    /// Its tag: the one written (`!!int` in full, `tag:yaml.org,2002:int`),
    /// else the one the document's dialect resolves. `None` for an alias.
    pub fn tag(&self) -> Option<&'t [u8]> {
        // SAFETY: tags are in the tree's pool or static
        let tag = unsafe { bytes(sys::oyl_node_tag(self.tree.raw(), self.i)) };
        (!tag.is_empty()).then_some(tag)
    }

    /// The tag as UTF-8.
    pub fn tag_str(&self) -> Option<Result<&'t str, Utf8Error>> {
        self.tag().map(std::str::from_utf8)
    }

    /// Its anchor name (`&name`), if it has one.
    pub fn anchor(&self) -> Option<&'t [u8]> {
        // SAFETY: anchors are in the tree's pool
        let anchor = unsafe { opt_bytes(sys::oyl_node_anchor(self.tree.raw(), self.i)) };
        anchor.filter(|a| !a.is_empty())
    }

    /// How a scalar was written.
    pub fn style(&self) -> ScalarStyle {
        ScalarStyle::from_raw(sys::oyl_scalar_style::from(self.raw().style))
    }

    /// A collection in flow style (`[...]`, `{...}`).
    pub fn is_flow(&self) -> bool {
        self.raw().flags & sys::OYL_NODE_FLOW != 0
    }

    /// A mapping's key.
    pub fn is_key(&self) -> bool {
        self.raw().flags & sys::OYL_NODE_KEY != 0
    }

    /// A scalar's value, typed by the document's dialect (through an
    /// alias). An error for a collection, or for a scalar its explicit tag
    /// rejects (`!!int abc`).
    pub fn value(&self) -> Result<Value<'t>, Error> {
        let mut v = std::mem::MaybeUninit::<sys::oyl_value>::uninit();
        let status = unsafe { sys::oyl_node_value(self.tree.raw(), self.i, v.as_mut_ptr()) };
        match status {
            sys::OYL_OK => {
                // SAFETY: on success the value is set, and its text is the
                // scalar's, which outlives 't
                Ok(unsafe { Value::from_raw(&v.assume_init(), self.target().text()) })
            }
            sys::OYL_ERR_INPUT => Err(self.error(ErrorKind::Input, "not a scalar")),
            sys::OYL_ERR_PARSE => {
                let tag = self.target().tag().map(String::from_utf8_lossy).unwrap_or_default();
                Err(self.error(ErrorKind::Parse, &format!("{tag} does not accept this value")))
            }
            _ => Err(Error::from_status(status)),
        }
    }

    /// Where it starts: offset, line and column.
    pub fn mark(&self) -> Mark {
        Mark::from(unsafe { sys::oyl_node_mark(self.tree.raw(), self.i) })
    }

    /// The bytes of the input it was read from, as offsets in the input;
    /// a block collection ends where its last child does.
    pub fn span(&self) -> Range<usize> {
        let base = self.tree.base();
        let n = self.raw();
        base + n.start as usize..base + n.end as usize
    }

    /// The node an alias refers to; any other node is itself. An alias may
    /// refer to a collection that contains it.
    pub fn target(&self) -> Node<'t> {
        let t = unsafe { sys::oyl_node_target(self.tree.raw(), self.i) };
        if t == sys::OYL_NO_NODE { *self } else { Node { tree: self.tree, i: t } }
    }

    /// The collection holding it; `None` for the root.
    pub fn parent(&self) -> Option<Node<'t>> {
        let up = self.raw().parent;
        (up != 0).then(|| Node { tree: self.tree, i: self.i - up })
    }

    /// The value of `key` in a mapping, or `None`. `key` is the string
    /// key; when the mapping has none, the key a scalar written as `key`
    /// is, so `"80"` finds `80`. Keys are equal when their tags and values
    /// are, as YAML compares them: `0x50` and `80` are one key, `80` and
    /// `"80"` two. A key written twice gives its last value; `<<` merges
    /// apply in the document's dialect's order.
    pub fn get(&self, key: impl AsRef<[u8]>) -> Option<Node<'t>> {
        let i = unsafe { sys::oyl_node_get(self.tree.raw(), self.i, raw_str(Some(key.as_ref()))) };
        (i != sys::OYL_NO_NODE).then_some(Node { tree: self.tree, i })
    }

    /// As [`get`](Node::get), for the key equal to `key`: a scalar of its
    /// kind's tag (`!!null`, `!!bool`, `!!int`, `!!float`, or for text
    /// `!!str`) with its value, so `Value::Int(80)` finds `80` and `0x50`
    /// but not `"80"`.
    pub fn get_value(&self, key: Value<'_>) -> Option<Node<'t>> {
        let raw = key.raw();
        let i = unsafe { sys::oyl_node_get_value(self.tree.raw(), self.i, &raw) };
        (i != sys::OYL_NO_NODE).then_some(Node { tree: self.tree, i })
    }

    /// Item `index` of a sequence, or `None`.
    pub fn at(&self, index: usize) -> Option<Node<'t>> {
        let k = u32::try_from(index).ok()?;
        let i = unsafe { sys::oyl_node_at(self.tree.raw(), self.i, k) };
        (i != sys::OYL_NO_NODE).then_some(Node { tree: self.tree, i })
    }

    /// A sequence's items or a mapping's pairs, as written; 0 for a scalar.
    pub fn len(&self) -> usize {
        let t = self.target();
        match t.kind() {
            NodeKind::Sequence | NodeKind::Mapping => t.raw().len as usize,
            _ => 0,
        }
    }

    /// No items or pairs (or not a collection).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A sequence's items, in order (none for anything else).
    pub fn items(&self) -> Items<'t> {
        let t = self.target();
        let n = t.raw();
        let count = if t.kind() == NodeKind::Sequence { n.len } else { 0 };
        Items { tree: self.tree, next: t.i + 1, left: count }
    }

    /// A mapping's pairs as written, keys and values (none for anything
    /// else). For the mapping as a program sees it, merges applied and each
    /// key once, use [`entries`](Node::entries).
    pub fn pairs(&self) -> Pairs<'t> {
        let t = self.target();
        let n = t.raw();
        let count = if t.kind() == NodeKind::Mapping { n.len } else { 0 };
        Pairs { tree: self.tree, next: t.i + 1, left: count }
    }

    /// A mapping as a program sees it: `<<` merges applied, each key once
    /// (its last value), in the order keys first appear. Scalar keys are
    /// equal when their tags and values are, so `1` and `"1"` are two keys
    /// and `0x1` and `1` one; collection keys are all kept.
    pub fn entries(&self) -> Result<Vec<(Node<'t>, Node<'t>)>, Error> {
        self.collect_entries(0)
    }

    /// As [`entries`](Node::entries), but a key written twice is an error,
    /// as YAML has it, with the repeated key's position: `true` and `True`
    /// are a repeat, `1` and `"1"` aren't.
    pub fn entries_strict(&self) -> Result<Vec<(Node<'t>, Node<'t>)>, Error> {
        self.collect_entries(sys::OYL_ENTRIES_STRICT)
    }

    fn collect_entries(&self, flags: std::ffi::c_uint) -> Result<Vec<(Node<'t>, Node<'t>)>, Error> {
        let arena = NonNull::new(unsafe { sys::oyl_arena_new(4096) }).ok_or_else(Error::memory)?;
        let out = self.entries_in(arena, flags);
        unsafe { sys::oyl_arena_free(arena.as_ptr()) };
        out
    }

    /// The entries, with the C pairs allocated in `arena` (which the caller
    /// may reset afterwards: the pairs are copied out).
    pub(crate) fn entries_in(&self, arena: NonNull<sys::oyl_arena>, flags: std::ffi::c_uint) -> Result<Vec<(Node<'t>, Node<'t>)>, Error> {
        let mut pairs: *const sys::oyl_pair = std::ptr::null();
        let mut n: usize = 0;
        let status = unsafe { sys::oyl_node_entries(self.tree.raw(), self.i, flags, arena.as_ptr(), &mut pairs, &mut n) };
        match status {
            sys::OYL_OK => {
                // SAFETY: on success `pairs` holds `n` pairs, in the arena
                let raw = if n == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(pairs, n) } };
                Ok(raw.iter().map(|p| (Node { tree: self.tree, i: p.key }, Node { tree: self.tree, i: p.value })).collect())
            }
            sys::OYL_ERR_PARSE => {
                // under STRICT, `n` is the repeated key's node
                let key = u32::try_from(n).ok().and_then(|i| self.tree.node(i as usize).map(|_| Node { tree: self.tree, i }));
                let mut e = self.error(ErrorKind::Parse, "key written twice");
                if let Some(key) = key {
                    e.mark = Some(key.mark());
                    e.message = format!("key '{}' written twice", String::from_utf8_lossy(key.target().text()));
                }
                Err(e)
            }
            sys::OYL_ERR_INPUT => Err(self.error(ErrorKind::Input, "not a mapping")),
            sys::OYL_ERR_LIMIT => Err(self.error(ErrorKind::Limit, "merges nested too deep or too costly")),
            _ => Err(Error::from_status(status)),
        }
    }

    fn error(&self, kind: ErrorKind, message: &str) -> Error {
        Error { kind, message: message.to_owned(), mark: Some(self.mark()) }
    }
}

impl PartialEq for Node<'_> {
    /// The same node of the same tree.
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.tree, other.tree) && self.i == other.i
    }
}

impl Eq for Node<'_> {}

impl fmt::Debug for Node<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("Node");
        d.field("index", &self.i).field("kind", &self.kind());
        match self.kind() {
            NodeKind::Scalar | NodeKind::Alias => d.field("text", &String::from_utf8_lossy(self.text())),
            _ => d.field("len", &self.len()),
        };
        d.finish()
    }
}

/// A sequence's items: [`Node::items`].
#[derive(Debug, Clone)]
pub struct Items<'t> {
    tree: &'t Tree<'t>,
    next: u32,
    left: u32,
}

impl<'t> Iterator for Items<'t> {
    type Item = Node<'t>;

    fn next(&mut self) -> Option<Node<'t>> {
        if self.left == 0 {
            return None;
        }
        let i = self.next;
        self.next += self.tree.raw_node(i).span;
        self.left -= 1;
        Some(Node { tree: self.tree, i })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.left as usize, Some(self.left as usize))
    }
}

impl ExactSizeIterator for Items<'_> {}
impl std::iter::FusedIterator for Items<'_> {}

/// A mapping's pairs as written: [`Node::pairs`].
#[derive(Debug, Clone)]
pub struct Pairs<'t> {
    tree: &'t Tree<'t>,
    next: u32,
    left: u32,
}

impl<'t> Iterator for Pairs<'t> {
    type Item = (Node<'t>, Node<'t>);

    fn next(&mut self) -> Option<(Node<'t>, Node<'t>)> {
        if self.left == 0 {
            return None;
        }
        let k = self.next;
        let v = k + self.tree.raw_node(k).span;
        self.next = v + self.tree.raw_node(v).span;
        self.left -= 1;
        Some((Node { tree: self.tree, i: k }, Node { tree: self.tree, i: v }))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.left as usize, Some(self.left as usize))
    }
}

impl ExactSizeIterator for Pairs<'_> {}
impl std::iter::FusedIterator for Pairs<'_> {}

// ── Reading trees ───────────────────────────────────────────

impl<'i> Parser<'i> {
    /// The next document as a [`Tree`]; `Ok(None)` once the stream has
    /// ended. Call it at a document's start (not between that document's
    /// events). The parser's dialect, schema and limits apply; leave
    /// [`merge`](Parser::merge) and [`resolve`](Parser::resolve) off, since
    /// the tree keeps aliases and merges as written.
    pub fn next_tree(&self) -> Result<Option<Tree<'i>>, Error> {
        if self.state.get() != State::Parsing {
            return Ok(None);
        }
        let mut raw: *mut sys::oyl_tree = std::ptr::null_mut();
        let status = unsafe { sys::oyl_parse_tree(self.parser.as_ptr(), &mut raw) };
        if status != sys::OYL_OK {
            self.state.set(State::Failed);
            return Err(self.error(status));
        }
        let Some(raw) = NonNull::new(raw) else {
            self.state.set(State::Done);
            return Ok(None);
        };
        let mut count = 0u32;
        let nodes = unsafe { sys::oyl_tree_nodes(raw.as_ptr(), &mut count) };
        if nodes.is_null() || count == 0 {
            unsafe { sys::oyl_tree_free(raw.as_ptr()) };
            self.state.set(State::Failed);
            return Err(Error::from_status(sys::OYL_ERR_INPUT));
        }
        Ok(Some(Tree { raw, nodes, count, _input: PhantomData }))
    }

    /// The documents as trees: an iterator of `Result<Tree, Error>` that
    /// ends after the stream does, or after an error.
    pub fn trees(&self) -> Trees<'_, 'i> {
        Trees { parser: self }
    }
}

/// The iterator [`Parser::trees`] returns.
#[derive(Debug)]
pub struct Trees<'p, 'i> {
    parser: &'p Parser<'i>,
}

impl<'i> Iterator for Trees<'_, 'i> {
    type Item = Result<Tree<'i>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.parser.next_tree().transpose()
    }
}

impl std::iter::FusedIterator for Trees<'_, '_> {}
