# oyl.rs

Rust bindings for [Oyl](https://github.com/openbohemians/oyl), the optimized
YAML library: a fast YAML 1.2 parser and emitter written in C11, with SIMD
scanning and zero-copy strings.

Two crates:

- **`oyl`**: the safe API. A pull parser whose events come from an iterator,
  errors that say where they happened, schema presets, and an emitter.
- **`oyl-sys`**: the raw C declarations, mirroring `oyl.h`. It compiles Oyl
  from sources vendored in the crate, so nothing needs to be installed; a C
  compiler is enough.

## Installation

```toml
[dependencies]
oyl = { git = "https://github.com/openbohemians/oyl.rs" }
```

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

`Emitter::event` re-emits a parser's events, so a document can be
reformatted (`style`, `indent`). The output always parses back to the same
data: a plain scalar stays plain only when it reads back as the same text.

`cargo run --example events -- file.yaml` prints a file's events.

## Updating the C library

`oyl-sys/oyl` holds a copy of Oyl's sources, and `oyl-sys/oyl/COMMIT` names
the commit they came from. To vendor another version, from a checkout of
Oyl:

```sh
scripts/sync-oyl.sh path/to/oyl          # the checkout's committed HEAD
scripts/sync-oyl.sh path/to/oyl v1.1.0   # or a release tag
cargo test
```

The script also sets the build metadata of `oyl-sys`'s version, the part
after `+`, to the vendored Oyl version: `0.1.0+1.0.0` bundles Oyl 1.0.0.
crates.io won't take two versions that differ only there, so publishing
newly vendored sources also needs a bump before the `+`.

`oyl-sys` checks that its Rust structs have the C structs' sizes and field
offsets, so a layout change in Oyl fails its tests rather than misreading
memory.

## License

MIT; Oyl's own license is in `oyl-sys/oyl/LICENSE`.
