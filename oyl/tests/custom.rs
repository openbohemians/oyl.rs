use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use oyl::{
    CustomDialect, Dialect, ErrorKind, EventKind, FloatFlags, IntFlags, JsonOptions, MergeOrder, Parser,
    ScalarStyle, Schema, SchemaBuilder, Syntax, Value, WarningKind,
};

const SIZE: &str = "tag:example.com,2000:size";

/// Sizes such as `4Ki`, as ints; `boom` panics.
fn sizes() -> SchemaBuilder {
    SchemaBuilder::new().custom(SIZE, |text: &[u8]| {
        assert_ne!(text, b"boom", "a panic in a custom type");
        let digits = text.strip_suffix(b"Ki")?;
        let n: i64 = std::str::from_utf8(digits).ok()?.parse().ok()?;
        Some(Value::Int(n * 1024))
    })
}

#[test]
fn a_built_schema_types_scalars() {
    let schema = SchemaBuilder::new()
        .nulls(&["null", ""])
        .bools(&["yes"], &["nope"])
        .word("red", "!color")
        .word_ignore_case("blue", "!color")
        .ints(IntFlags::SIGN | IntFlags::HEX)
        .floats(FloatFlags::NONE)
        .build()
        .unwrap();
    let s = schema.schema();
    assert_eq!(s.resolve(b"nope", ScalarStyle::Plain), b"tag:yaml.org,2002:bool");
    assert_eq!(s.resolve(b"BLUE", ScalarStyle::Plain), b"!color");
    assert_eq!(s.resolve(b"Red", ScalarStyle::Plain), b"tag:yaml.org,2002:str", "red is exact");
    assert_eq!(s.resolve(b"true", ScalarStyle::Plain), b"tag:yaml.org,2002:str", "not one of its words");
    assert_eq!(s.resolve(b"0x1F", ScalarStyle::Plain), b"tag:yaml.org,2002:int");
    assert_eq!(s.resolve(b"0o17", ScalarStyle::Plain), b"tag:yaml.org,2002:str", "no octal");
    assert_eq!(s.resolve(b"yes", ScalarStyle::SingleQuoted), b"tag:yaml.org,2002:str");
    assert_eq!(s.value(b"nope", ScalarStyle::Plain, None).unwrap(), Value::Bool(false));
    assert_eq!(s.value(b"", ScalarStyle::Plain, None).unwrap(), Value::Null);
    assert_eq!(s.value(b"-0x10", ScalarStyle::Plain, None).unwrap(), Value::Str(b"-0x10"), "no sign before a prefix");
    assert_eq!(s.value(b"1.5", ScalarStyle::Plain, None).unwrap(), Value::Float(1.5));
    assert_eq!(s.value(b"blue", ScalarStyle::Plain, None).unwrap(), Value::Str(b"blue"));

    // events and trees read with it
    let mut parser = Parser::new("a: yes\nb: red\n").unwrap();
    parser.schema(s);
    let tags: Vec<_> = parser.events().map(|e| e.unwrap()).filter_map(|e| e.tag.map(<[u8]>::to_vec)).collect();
    assert!(tags.contains(&b"!color".to_vec()) && tags.contains(&b"tag:yaml.org,2002:bool".to_vec()), "{tags:?}");
    let mut parser = Parser::new("a: yes\nb: 0x10\n").unwrap();
    parser.schema(s);
    let tree = parser.next_tree().unwrap().unwrap();
    assert_eq!(tree.root().get("a").unwrap().value().unwrap(), Value::Bool(true));
    assert_eq!(tree.root().get("b").unwrap().value().unwrap(), Value::Int(16));
    assert_eq!(tree.dialect().schema(), s, "a schema alone makes the document's dialect");

    // and JSON
    let parser = Parser::new("a: yes\nb: nope\nc: 0x10\n").unwrap();
    let json = parser.next_json(&JsonOptions { schema: Some(s), ..JsonOptions::default() }).unwrap();
    assert_eq!(json, Some(r#"{"a":true,"b":false,"c":16}"#));
}

#[test]
fn a_word_with_a_nul_byte_is_an_error() {
    let err = SchemaBuilder::new().nulls(&["a\0b"]).build().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Input);
}

#[test]
fn custom_types() {
    let schema = sizes().ints(IntFlags::CORE).build().unwrap();
    let mut parser = Parser::new("a: 4Ki\nb: 4\nc: !<tag:example.com,2000:size> 2Ki\nd: !<tag:example.com,2000:size> x\n").unwrap();
    parser.schema(schema.schema());
    let tree = parser.next_tree().unwrap().unwrap();
    let root = tree.root();
    assert_eq!(root.get("a").unwrap().value().unwrap(), Value::Int(4096));
    assert_eq!(root.get("a").unwrap().tag(), Some(SIZE.as_bytes()));
    assert_eq!(root.get("b").unwrap().value().unwrap(), Value::Int(4), "declined, so the next rule");
    assert_eq!(root.get("c").unwrap().value().unwrap(), Value::Int(2048), "an explicit tag");
    assert_eq!(root.get("d").unwrap().value().unwrap_err().kind(), ErrorKind::Parse, "its tag declines it");
}

#[test]
fn a_custom_type_gives_text_from_its_text() {
    let schema = SchemaBuilder::new()
        .custom("!quoted", |text: &[u8]| text.strip_prefix(b"=").map(Value::Str))
        .build()
        .unwrap();
    let mut parser = Parser::new("a: =x\nb: y\n").unwrap();
    parser.schema(schema.schema());
    let tree = parser.next_tree().unwrap().unwrap();
    assert_eq!(tree.root().get("a").unwrap().value().unwrap(), Value::Str(b"x"));
    assert_eq!(tree.root().get("b").unwrap().value().unwrap(), Value::Str(b"y"));
}

#[test]
fn a_panic_in_a_custom_type_goes_on_from_the_call() {
    let schema = sizes().build().unwrap();
    // while building the tree, which types its scalars
    let mut parser = Parser::new("a: boom\n").unwrap();
    parser.schema(schema.schema());
    assert!(catch_unwind(AssertUnwindSafe(|| parser.next_tree())).is_err());
    // while reading a value
    let value = || schema.schema().value(b"boom", ScalarStyle::Plain, Some(SIZE.as_bytes())).map(|_| ());
    assert!(catch_unwind(AssertUnwindSafe(value)).is_err());
    // and nothing is left over for the next call
    assert_eq!(schema.schema().value(b"1Ki", ScalarStyle::Plain, None).unwrap(), Value::Int(1024));
    let mut parser = Parser::new("a: 1Ki\n").unwrap();
    parser.schema(schema.schema());
    assert_eq!(parser.next_tree().unwrap().unwrap().root().get("a").unwrap().value().unwrap(), Value::Int(1024));
}

#[test]
fn a_schema_is_shared_between_threads() {
    let schema = sizes().build().unwrap();
    std::thread::scope(|scope| {
        for i in 1..=4 {
            let s = schema.schema();
            scope.spawn(move || {
                let input = format!("a: {i}Ki\n");
                let mut parser = Parser::new(&input).unwrap();
                parser.schema(s);
                let tree = parser.next_tree().unwrap().unwrap();
                assert_eq!(tree.root().get("a").unwrap().value().unwrap(), Value::Int(i * 1024));
            });
        }
    });
}

#[test]
fn custom_dialects() {
    let d = CustomDialect::new((1, 1), Schema::goyaml2(), Syntax::EOF_NO_BREAK, MergeOrder::None).unwrap();
    let dialect = d.dialect();
    assert_eq!(dialect.version(), (1, 1));
    assert_eq!(dialect.syntax(), Syntax::EOF_NO_BREAK);
    assert!(dialect.syntax().contains(Syntax::EOF_NO_BREAK) && !dialect.syntax().contains(Syntax::GOYAML2));
    assert_eq!(dialect.merge_order(), MergeOrder::None);
    assert_eq!(dialect.schema(), Schema::goyaml2());
    assert_eq!(format!("{dialect:?}"), "Dialect(custom)");
    assert_eq!(Dialect::goyaml2().syntax(), Syntax::GOYAML2);

    let mut parser = Parser::new("on: yes\nm: {<<: {x: 1}}\ns: |\n  text").unwrap();
    parser.dialect(dialect);
    let tree = parser.next_tree().unwrap().unwrap();
    let root = tree.root();
    assert_eq!(tree.dialect(), dialect);
    assert_eq!(root.get("on").unwrap().value().unwrap(), Value::Bool(true), "go-yaml v2's types");
    assert!(root.get("m").unwrap().get("x").is_none(), "no merges");
    assert_eq!(root.get("s").unwrap().text(), b"text", "no line feed at the end of the input");

    for bad in [(2, 0), (1, u32::MAX)] {
        let err = CustomDialect::new(bad, Schema::core(), Syntax::NONE, MergeOrder::Yaml).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Input, "{bad:?}");
    }
}

#[test]
fn a_custom_dialect_with_a_custom_schema() {
    let schema = sizes().build().unwrap();
    let d = CustomDialect::new((1, 2), schema.schema(), Syntax::NONE, MergeOrder::Yaml).unwrap();
    let value: std::collections::BTreeMap<String, i64> = oyl::from_str_with("a: 2Ki\n", d.dialect()).unwrap();
    assert_eq!(value["a"], 2048);
}

#[test]
fn documents_choose_dialects_by_version() {
    let d = CustomDialect::new((1, 1), Schema::core(), Syntax::NONE, MergeOrder::Ordered).unwrap();
    let input = "%YAML 1.1\n---\na: yes\n...\n---\nb: yes\n";
    let mut parser = Parser::new(input).unwrap();
    parser.dialect_for_version((1, 1), d.dialect()).unwrap();
    assert_eq!(parser.document_dialect(), Dialect::yaml12(), "before the first document");
    let mut dialects = Vec::new();
    for event in parser.events() {
        if event.unwrap().kind == EventKind::DocumentStart {
            dialects.push(parser.document_dialect().merge_order());
        }
    }
    assert_eq!(dialects, [MergeOrder::Ordered, MergeOrder::Yaml], "the given one, then the default");

    let mut parser = Parser::new(input).unwrap();
    parser.dialect_for_version((1, 1), d.dialect()).unwrap();
    let trees: Vec<_> = parser.trees().map(|t| t.unwrap()).collect();
    assert_eq!(trees[0].dialect(), d.dialect());
    assert_eq!(trees[0].root().get("a").unwrap().value().unwrap(), Value::Str(b"yes"), "its schema is Core");
    assert_eq!(trees[1].dialect(), Dialect::yaml12());

    let mut parser = Parser::new("").unwrap();
    assert_eq!(parser.dialect_for_version((2, 0), d.dialect()).unwrap_err().kind(), ErrorKind::Input);
    for minor in 0..8 {
        parser.dialect_for_version((1, minor), d.dialect()).unwrap();
    }
    parser.dialect_for_version((1, 3), d.dialect()).expect("a version again replaces");
    assert_eq!(parser.dialect_for_version((1, 8), d.dialect()).unwrap_err().kind(), ErrorKind::Limit);
}

#[test]
fn documents_choose_dialects_by_tag_prefix() {
    let schema = sizes().build().unwrap();
    let d = CustomDialect::new((1, 2), schema.schema(), Syntax::NONE, MergeOrder::Yaml).unwrap();
    let input = "%TAG !! tag:example.com,2000:\n---\nmemory: 4Ki\n";
    let mut parser = Parser::new(input).unwrap();
    parser.dialect_for_tag_prefix("tag:example.com,2000:", d.dialect()).unwrap();
    let tree = parser.next_tree().unwrap().unwrap();
    assert_eq!(tree.dialect(), d.dialect());
    assert_eq!(tree.root().get("memory").unwrap().value().unwrap(), Value::Int(4096));

    // a prefix nobody gave a dialect for is a warning
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let mut parser = Parser::new(input).unwrap();
    parser.on_warning(move |w| log.lock().unwrap().push((w.kind, w.mark.line)));
    let tree = parser.next_tree().unwrap().unwrap();
    assert_eq!(tree.dialect(), Dialect::yaml12());
    assert_eq!(*seen.lock().unwrap(), [(WarningKind::Dialect, 2)]);

    let mut parser = Parser::new("").unwrap();
    let long = "x".repeat(256);
    assert_eq!(parser.dialect_for_tag_prefix(&long, d.dialect()).unwrap_err().kind(), ErrorKind::Input);
    assert_eq!(parser.dialect_for_tag_prefix("a\0b", d.dialect()).unwrap_err().kind(), ErrorKind::Input);
}

#[test]
fn warnings() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let mut parser = Parser::new("a: \"it\\'s\"\n").unwrap();
    parser.dialect(Dialect::goyaml2()).on_warning(move |w| log.lock().unwrap().push(w.clone()));
    let tree = parser.next_tree().unwrap().unwrap();
    assert_eq!(tree.root().get("a").unwrap().text(), b"it's");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].kind, WarningKind::Leniency);
    assert!(!seen[0].message.is_empty() && seen[0].to_string().contains("line 1"), "{}", seen[0]);

    // a handler set again replaces the one before
    let count = Arc::new(Mutex::new((0, 0)));
    let (first, second) = (Arc::clone(&count), Arc::clone(&count));
    let mut parser = Parser::new("%YAML 1.5\n--- a\n").unwrap();
    parser.on_warning(move |_| first.lock().unwrap().0 += 1);
    parser.on_warning(move |_| second.lock().unwrap().1 += 1);
    parser.next_tree().unwrap();
    assert_eq!(*count.lock().unwrap(), (0, 1));
}

#[test]
fn a_panic_in_a_warning_handler_goes_on_from_the_call() {
    let mut parser = Parser::new("%YAML 1.5\n--- a\n").unwrap();
    parser.on_warning(|w| panic!("{w}"));
    assert!(catch_unwind(AssertUnwindSafe(|| parser.next_tree().map(|_| ()))).is_err());
    let mut parser = Parser::new("%YAML 1.5\n--- a\n").unwrap();
    parser.on_warning(|w| panic!("{w}"));
    let kinds: Vec<_> = std::iter::from_fn(|| catch_unwind(AssertUnwindSafe(|| parser.next_event().map(|e| e.map(|e| e.kind)))).ok())
        .take(3)
        .collect();
    assert!(kinds.len() < 3, "an event's call panicked: {kinds:?}");
}

#[test]
fn a_parser_may_outlive_nothing_given_to_it() {
    // what the compile_fail examples on CustomSchema and CustomDialect
    // check, but with everything alive
    let schema = SchemaBuilder::new().build().unwrap();
    let mut parser = Parser::new("a").unwrap();
    parser.schema(schema.schema());
    parser.next_tree().unwrap();
    let d = CustomDialect::new((1, 2), Schema::core(), Syntax::NONE, MergeOrder::Yaml).unwrap();
    let tree = {
        let mut p = Parser::new("a").unwrap();
        p.dialect(d.dialect());
        p.next_tree().unwrap().unwrap()
    };
    tree.root();
}

#[test]
fn custom_schemas_and_dialects_are_send_and_sync() {
    fn both<T: Send + Sync>() {}
    both::<oyl::CustomSchema>();
    both::<CustomDialect<'static>>();
    both::<Schema<'static>>();
    fn send<T: Send>() {}
    send::<Parser<'static>>();
}
