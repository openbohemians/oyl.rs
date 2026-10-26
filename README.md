# oyl.rs

Rust bindings for [Oyl](https://github.com/openbohemians/oyl), the optimized
YAML library: a fast YAML 1.2 parser and emitter written in C11, with SIMD
scanning and zero-copy strings.

Two crates:

- **`oyl`**: the safe API. A pull parser whose events come from an iterator,
  documents as trees with lookups and typed values, dialects (YAML 1.2,
  YAML 1.1, go-yaml v2 as Kubernetes reads YAML), YAML to JSON, errors that
  say where they happened, and an emitter that keeps documents' `%YAML`
  and `%TAG` directives.
- **`oyl-sys`**: the raw C declarations, mirroring `oyl.h`. It compiles Oyl
  from sources vendored in the crate, so nothing needs to be installed; a C
  compiler is enough.

## Installation

```toml
[dependencies]
oyl = "0.2"
```

Oyl's C builds with GCC or Clang, so the crates work on Linux and macOS.
Windows isn't supported yet: Microsoft's C compiler, which Rust uses there
by default, can't compile it.

## Parsing

```rust
use oyl::{EventKind, Parser};

let parser = Parser::new("name: oyl\nlanguages: [C, Rust]\n")?;
for event in parser.events() {
    let event = event?;
    if event.kind == EventKind::Scalar {
        println!("{} at line {}", event.value_str()?, event.start.line);
    }
}
```

Events hold the scalar value or alias name, the anchor and tag, the scalar
style, whether a collection is in flow style, and start and end positions.
Their strings are byte slices into the input, or into the parser's own
memory for scalars it had to rewrite (escapes, folding), so they cost no
copy and stay valid as long as the parser.

Options are set before the first event:

```rust
use oyl::{Parser, Schema};

let mut parser = Parser::new(&input)?;
parser
    .merge(true)            // expand <<: merge keys
    .resolve(true)          // replace aliases with their anchored nodes
    .schema(Schema::core()) // tag untagged scalars: 42 is tag:yaml.org,2002:int
    .max_events(1_000_000); // safety limits: 10,000 events and depth 256 by default
```

Errors carry a kind (`Parse`, `Scan`, `Limit`, ...), Oyl's message and,
for parse errors, the position. For `"a: 1\nb: [2, 3\n"`:

```text
expected ',' or ']' in flow sequence at line 3, column 1
```

Below the parser, `Scanner` gives YAML's tokens (indicators, scalars,
tags, anchors, directives) with their positions, for syntax highlighting
and the like.

## Trees

A tree holds a document in memory: look things up, walk it, and read
typed values, without handling events.

```rust
use oyl::{Parser, Value};

let parser = Parser::new("base: &b {replicas: 2}\nweb:\n  <<: *b\n  image: nginx\n")?;
let tree = parser.next_tree()?.expect("a document");
let web = tree.root().get("web").expect("a key");
assert_eq!(web.get("image").unwrap().text(), b"nginx");
assert_eq!(web.get("replicas").unwrap().value()?, Value::Int(2));
for (key, value) in web.entries()? {          // merges applied, each key once
    println!("{} = {:?}", key.text_str()?, value.value()?);
}
```

`next_tree` reads one document (`trees()` iterates over them). A `Node` is
a cheap handle: its kind, text, tag, anchor, style, typed `value`, `mark`
and `span` (the input it was read from, for edits that keep formatting),
`get` and `at`, `items` and `pairs` as written, and `entries` as a program
sees the mapping. Aliases and `<<` merge keys stay as written in the tree;
`get` and `entries` follow aliases and apply merges in the order of the
document's dialect. Keys are equal as YAML compares them, by tag and value:
`0x50` and `80` are one key, `80` and `"80"` two. `get` takes a key's text
(`"80"` finds the string, else `80`), `get_value` its value.
`entries_strict` makes a repeated key an error with its position.

Text stays in the input, except what the parser rewrote (escapes, folded
lines), which the tree keeps: a tree borrows the input, not the parser, so
it can outlive the parser, and threads can share it. `raw_nodes()`,
`raw_props()` and `raw_pool()` give Oyl's arrays (32-byte nodes, their tags
and anchors, and the tree's own text) for walking without calls, with
their types and constants in `oyl::sys`. `Emitter::tree` writes a tree
back out as YAML.

## serde

With the `serde` feature, `oyl::from_str` deserializes YAML into your
types:

```toml
[dependencies]
oyl = { version = "0.2", features = ["serde"] }
serde = { version = "1", features = ["derive"] }
```

```rust
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Container<'a> {
    name: &'a str,
    image: &'a str,
    image_pull_policy: Option<String>,
}

let c: Container = oyl::from_str("name: web\nimage: nginx:1.27\n")?;
```

The YAML is read into a tree and deserialized from it:

- Scalars are typed by YAML 1.2 by default; `from_str_with(input,
  Dialect::goyaml2())` reads as Kubernetes does (`paused: no` is a bool).
  A string field takes any scalar's text (`version: 1.20` is `"1.20"`).
- `&str` fields borrow from the input when the parser didn't rewrite the
  text (escapes, folded lines); `String` or `Cow<str>` take any.
- Aliases are followed and `<<` merges applied in the dialect's order. A
  key written twice is an error, whatever the target type.
- Enums are a string (a unit variant), a mapping with one key, or a
  `!Variant` tag, as serde_yaml writes them.
- Errors give the line and column of the node that didn't fit. Nesting
  past 128 and alias bombs are errors, not stack overflows.
- `from_node` deserializes any node of a tree you hold, so a stream's
  documents can be read one by one (`Parser::trees`), or part of one.

On 848 public Kubernetes manifests, deserializing is 2.3x faster than
serde_yaml 0.9 into a generic value (every node visited) and 4.2x faster
into a typed struct (measured on one core of an Intel Core Ultra 7 155H).
serde_yaml was archived in 2024.

## Dialects

A dialect decides which types scalars get and how `<<` merges: YAML 1.2's
core schema by default; YAML 1.1 with PyYAML's types and merge (`yes` and
`on` are booleans, `0777` is octal), so a document PyYAML reads gets the
same values (with two exceptions, in `Dialect::yaml11`'s docs); or go-yaml
v2's, as Kubernetes reads YAML. A document's `%YAML 1.1` or `%YAML 1.2`
chooses that version's dialect when the parser's is of another version
(go-yaml v2's refuses any but 1.1, as go-yaml v2 does).

```rust
use oyl::{Dialect, Parser, Value};

let mut parser = Parser::new("enabled: yes\nmode: 0644\n")?;
parser.dialect(Dialect::goyaml2());
let tree = parser.next_tree()?.unwrap();
assert_eq!(tree.root().get("enabled").unwrap().value()?, Value::Bool(true));
assert_eq!(tree.root().get("mode").unwrap().value()?, Value::Int(0o644));
```

A program can build its own: a schema from words, the built-in number
parsers with their options, and types of its own (`SchemaBuilder`), and a
dialect from a schema, syntax leniencies and a merge order
(`CustomDialect`). Here YAML 1.2 with sizes such as `4Ki` as a type, and
`<<` an ordinary key:

```rust
use oyl::{CustomDialect, FloatFlags, IntFlags, MergeOrder, Parser, SchemaBuilder, Syntax, Value};

let schema = SchemaBuilder::new()
    .nulls(&["null", "~", ""])
    .bools(&["true"], &["false"])
    .custom("tag:example.com,2000:size", |text: &[u8]| {
        let n: i64 = std::str::from_utf8(text.strip_suffix(b"Ki")?).ok()?.parse().ok()?;
        Some(Value::Int(n * 1024))
    })
    .ints(IntFlags::CORE)
    .floats(FloatFlags::CORE)
    .build()?;
let dialect = CustomDialect::new((1, 2), schema.schema(), Syntax::NONE, MergeOrder::None)?;
let mut parser = Parser::new("memory: 4Ki\n")?;
parser.dialect(dialect.dialect());
let tree = parser.next_tree()?.unwrap();
assert_eq!(tree.root().get("memory").unwrap().value()?, Value::Int(4096));
```

A parser, and the trees it reads, can't outlive a schema or dialect given
to it. Documents can also choose a dialect by their `%YAML` version
(`Parser::dialect_for_version`) or by a `%TAG !!` prefix
(`Parser::dialect_for_tag_prefix`), and `Parser::document_dialect` says
which one a document got. `Parser::on_warning` reports what the parser
read but YAML 1.2 would read otherwise: a leniency of the dialect, a
`%YAML` version it reads as another, a `%TAG !!` prefix with no dialect.

## YAML to JSON

`next_json` writes the next document as JSON. With
`JsonOptions::kubernetes()` the output is byte for byte what
sigs.k8s.io/yaml's `YAMLToJSON` writes:

```rust
use oyl::{JsonOptions, Parser};

let parser = Parser::new("b: yes\na: 0777\n")?;
assert_eq!(parser.next_json(&JsonOptions::kubernetes())?, Some(r#"{"a":511,"b":true}"#));
```

## Emitting

```rust
use oyl::{Emitter, ScalarStyle};

let mut e = Emitter::new()?;
e.stream_start()?;
e.document_start(true)?;
e.mapping_start(None, None, false)?;
e.scalar("name", ScalarStyle::Plain, None, None)?;
e.scalar("oyl", ScalarStyle::Plain, None, None)?;
e.mapping_end()?;
e.document_end(true)?;
e.stream_end()?;
assert_eq!(e.output(), b"name: oyl\n");
```

`Emitter::reemit` re-emits a parser's events, each document with its
`%YAML` and `%TAG` directives, so a document can be reformatted (`style`,
`indent`); `Emitter::tree` writes a tree with its directives. To emit
events one at a time, give `Parser::directives` to `Emitter::directives`
at each document's start, then `Emitter::event`.

The output reads back as the events given: a plain scalar stays plain
only when it reads back as the same text, a document's tags are written
with its `%TAG` handles, and so on. Events out of order, and names or
directives YAML can't hold, are `ErrorKind::Emit` errors; an error is
final, so every later call returns it.

`cargo run --example events -- file.yaml` prints a file's events.

## Updating the C library

`oyl-sys/oyl` holds a copy of Oyl's sources, and `oyl-sys/oyl/COMMIT` names
the commit they came from. To vendor another version, from a checkout of
Oyl:

```sh
scripts/sync-oyl.sh path/to/oyl          # the checkout's committed HEAD
scripts/sync-oyl.sh path/to/oyl v1.2.0   # or a release tag
cargo test
```

The script also sets the build metadata of `oyl-sys`'s version, the part
after `+`, to the vendored Oyl version: `0.2.0+1.2.0` bundles Oyl 1.2.0.
crates.io won't take two versions that differ only there, so publishing
newly vendored sources also needs a bump before the `+`.

`oyl-sys` checks that its Rust structs have the C structs' sizes and field
offsets, so a layout change in Oyl fails its tests rather than misreading
memory.

## License

MIT; Oyl's own license is in `oyl-sys/oyl/LICENSE`.
