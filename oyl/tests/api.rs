use oyl::{Directives, EmitStyle, Emitter, ErrorKind, Event, EventKind, Mark, Parser, ScalarStyle, Schema};

fn kinds_and_values(parser: &Parser) -> Vec<(EventKind, String)> {
    parser
        .events()
        .map(|e| {
            let e = e.expect("parses");
            (e.kind, String::from_utf8(e.value.to_vec()).unwrap())
        })
        .collect()
}

fn scalars(input: &str, setup: impl FnOnce(&mut Parser)) -> Vec<String> {
    let mut parser = Parser::new(input).unwrap();
    setup(&mut parser);
    let out = parser
        .events()
        .map(|e| e.unwrap())
        .filter(|e| e.kind == EventKind::Scalar)
        .map(|e| e.value_str().unwrap().to_owned())
        .collect();
    out
}

#[test]
fn events_of_a_mapping() {
    use EventKind::*;
    let parser = Parser::new("a: 1\nb: [x, y]\n").unwrap();
    let got = kinds_and_values(&parser);
    let want: Vec<(EventKind, &str)> = vec![
        (StreamStart, ""), (DocumentStart, ""), (MappingStart, ""),
        (Scalar, "a"), (Scalar, "1"), (Scalar, "b"),
        (SequenceStart, ""), (Scalar, "x"), (Scalar, "y"), (SequenceEnd, ""),
        (MappingEnd, ""), (DocumentEnd, ""), (StreamEnd, ""),
    ];
    assert_eq!(got.len(), want.len());
    for ((k, v), (wk, wv)) in got.iter().zip(&want) {
        assert_eq!((k, v.as_str()), (wk, *wv));
    }
    assert!(parser.next_event().unwrap().is_none(), "nothing after the stream ends");
}

#[test]
fn marks_styles_and_flags() {
    let parser = Parser::new("key: 'quoted'\nlist: [1]\n").unwrap();
    let events: Vec<Event> = parser.events().map(|e| e.unwrap()).collect();
    let quoted = events.iter().find(|e| e.value == b"quoted").unwrap();
    assert_eq!(quoted.style, ScalarStyle::SingleQuoted);
    assert_eq!(quoted.start, Mark { offset: 5, line: 1, column: 6 });
    assert_eq!(quoted.end, Mark { offset: 13, line: 1, column: 14 });
    let seq = events.iter().find(|e| e.kind == EventKind::SequenceStart).unwrap();
    assert!(seq.flow);
    assert_eq!(seq.start.line, 2);
    let doc = events.iter().find(|e| e.kind == EventKind::DocumentStart).unwrap();
    assert!(doc.implicit);
}

#[test]
fn documents() {
    let parser = Parser::new("--- a\n--- b\n...\n").unwrap();
    let docs: Vec<Event> = parser
        .events()
        .map(|e| e.unwrap())
        .filter(|e| matches!(e.kind, EventKind::DocumentStart | EventKind::DocumentEnd))
        .collect();
    assert_eq!(docs.len(), 4);
    assert!(docs.iter().filter(|e| e.kind == EventKind::DocumentStart).all(|e| !e.implicit));
    assert!(!docs[3].implicit, "the second document ends with ...");
}

#[test]
fn parse_errors_have_a_position_and_end_the_events() {
    let parser = Parser::new("a: 1\nb: [2, 3\n").unwrap();
    let mut events = parser.events();
    let err = events.by_ref().find_map(|e| e.err()).expect("an error");
    assert!(matches!(err.kind(), ErrorKind::Parse | ErrorKind::Scan), "{err:?}");
    assert!(!err.message().is_empty());
    let mark = err.mark().expect("a position");
    assert!(mark.line >= 2, "{mark:?}");
    assert!(err.to_string().contains("line"));
    assert!(events.next().is_none(), "no events after an error");
}

#[test]
fn event_limit() {
    // 12 events: stream, document, sequence, 6 scalars
    let mut parser = Parser::new("[a, b, c, d, e, f]").unwrap();
    parser.max_events(5);
    let err = parser.events().find_map(|e| e.err()).expect("the limit");
    assert_eq!(err.kind(), ErrorKind::Limit);
    for max in [12, 0] {
        assert_eq!(scalars("[a, b, c, d, e, f]", |p| { p.max_events(max); }).len(), 6);
    }
}

#[test]
fn depth_limit() {
    let mut parser = Parser::new("[[[[x]]]]").unwrap();
    parser.max_depth(3);
    assert_eq!(parser.events().find_map(|e| e.err()).unwrap().kind(), ErrorKind::Limit);
    assert_eq!(scalars("[[[[x]]]]", |p| { p.max_depth(4); }), ["x"]);
}

#[test]
fn merge_keys() {
    let yaml = "base: &b {x: 1}\nderived:\n  <<: *b\n  y: 2\n";
    let merged = scalars(yaml, |p| { p.merge(true); });
    // the derived mapping holds both pairs (in no particular order)
    let derived = &merged[4..];
    let mut pairs: Vec<(&str, &str)> = derived.chunks(2).map(|kv| (kv[0].as_str(), kv[1].as_str())).collect();
    pairs.sort();
    assert_eq!(&merged[..4], ["base", "x", "1", "derived"]);
    assert_eq!(pairs, [("x", "1"), ("y", "2")]);
    assert!(scalars(yaml, |_| {}).contains(&"<<".to_owned()), "off by default");
}

#[test]
fn alias_resolution() {
    let yaml = "a: &x [1]\nb: *x\n";
    let aliases = |resolve| {
        let mut parser = Parser::new(yaml).unwrap();
        parser.resolve(resolve);
        let names: Vec<String> = parser
            .events()
            .map(|e| e.unwrap())
            .filter(|e| e.kind == EventKind::Alias)
            .map(|e| e.value_str().unwrap().to_owned())
            .collect();
        names
    };
    assert_eq!(aliases(false), ["x"]);
    assert!(aliases(true).is_empty());
    assert_eq!(scalars(yaml, |p| { p.resolve(true); }), ["a", "1", "b", "1"]);
}

#[test]
fn schemas() {
    let mut parser = Parser::new("int: 42\nbool: true\nstr: '42'\n").unwrap();
    parser.schema(Schema::core());
    let tags: Vec<(String, String)> = parser
        .events()
        .map(|e| e.unwrap())
        .filter(|e| e.kind == EventKind::Scalar)
        .map(|e| (e.value_str().unwrap().to_owned(), e.tag_str().unwrap().unwrap().to_owned()))
        .collect();
    assert_eq!(tags[1], ("42".into(), "tag:yaml.org,2002:int".into()));
    assert_eq!(tags[3], ("true".into(), "tag:yaml.org,2002:bool".into()));
    assert_eq!(tags[5], ("42".into(), "tag:yaml.org,2002:str".into()));
    assert_eq!(Schema::core().resolve(b"0x1F", ScalarStyle::Plain), b"tag:yaml.org,2002:int");
    assert_eq!(Schema::failsafe().resolve(b"42", ScalarStyle::Plain), b"tag:yaml.org,2002:str");
}

#[test]
fn events_outlive_later_calls() {
    let input = String::from("a: \"esc\\taped\"\nb: plain\n");
    let parser = Parser::new(&input).unwrap();
    let all: Vec<Event> = parser.events().collect::<Result<_, _>>().unwrap();
    // values from the input and from the parser's own memory (the
    // unescaped string) are still valid after parsing has finished
    let values: Vec<&[u8]> = all.iter().filter(|e| e.kind == EventKind::Scalar).map(|e| e.value).collect();
    assert_eq!(values, [&b"a"[..], b"esc\taped", b"b", b"plain"]);
}

#[test]
fn bytes_and_strings_as_input() {
    let owned: Vec<u8> = b"- x\n".to_vec();
    assert_eq!(Parser::new(&owned).unwrap().events().count(), 7);
    let text = String::from("- x\n");
    assert_eq!(Parser::new(&text).unwrap().events().count(), 7);
    assert_eq!(Parser::new("").unwrap().events().count(), 2);
}

#[test]
fn emitter_builds_yaml() {
    let mut e = Emitter::new().unwrap();
    e.stream_start().unwrap();
    e.document_start(true).unwrap();
    e.mapping_start(None, None, false).unwrap();
    e.scalar("name", ScalarStyle::Plain, None, None).unwrap();
    e.scalar("oyl", ScalarStyle::Plain, None, None).unwrap();
    e.scalar("tags", ScalarStyle::Plain, None, None).unwrap();
    e.sequence_start(Some("t"), None, true).unwrap();
    e.scalar("fast", ScalarStyle::Plain, None, None).unwrap();
    e.scalar("true", ScalarStyle::DoubleQuoted, None, None).unwrap(); // the string, not the bool
    e.sequence_end().unwrap();
    e.scalar("again", ScalarStyle::Plain, None, None).unwrap();
    e.alias("t").unwrap();
    e.mapping_end().unwrap();
    e.document_end(true).unwrap();
    e.stream_end().unwrap();
    let out = String::from_utf8(e.output()).unwrap();
    assert_eq!(scalars(&out, |_| {}), ["name", "oyl", "tags", "fast", "true", "again"], "{out}");
    let parser = Parser::new(&out).unwrap();
    let quoted = parser.events().map(|e| e.unwrap()).find(|e| e.value == b"true").unwrap();
    assert_ne!(quoted.style, ScalarStyle::Plain, "still a string: {out}");
}

#[test]
fn emitter_reemits_parsed_events() {
    let yaml = "a: &x [1, 2]\nb: *x\nc: |\n  text\n";
    for style in [EmitStyle::Block, EmitStyle::Flow, EmitStyle::Minimal] {
        let parser = Parser::new(yaml).unwrap();
        let mut e = Emitter::new().unwrap();
        e.style(style);
        for event in parser.events() {
            e.event(&event.unwrap()).unwrap();
        }
        let out = e.output();
        let before: Vec<_> = kinds_and_values(&Parser::new(yaml).unwrap());
        let after: Vec<_> = kinds_and_values(&Parser::new(&out).unwrap());
        assert_eq!(before, after, "{style:?}: {}", String::from_utf8_lossy(&out));
    }
}

#[test]
fn emitter_rejects_events_out_of_order() {
    let mut e = Emitter::new().unwrap();
    e.stream_start().unwrap();
    assert_eq!(e.mapping_end().unwrap_err().kind(), ErrorKind::Emit);
}

#[test]
fn emitter_errors_are_final() {
    let mut e = Emitter::new().unwrap();
    e.stream_start().unwrap();
    assert_eq!(e.alias("a b").unwrap_err().kind(), ErrorKind::Emit, "a name YAML can't hold");
    assert_eq!(e.document_start(true).unwrap_err().kind(), ErrorKind::Emit, "every later call fails");
    assert_eq!(e.stream_end().unwrap_err().kind(), ErrorKind::Emit);
}

#[test]
fn emitter_rejects_directives_yaml_cant_take() {
    let fails = |f: &dyn Fn(&mut Emitter) -> Result<(), oyl::Error>| {
        let mut e = Emitter::new().unwrap();
        e.stream_start().unwrap();
        f(&mut e).unwrap_err().kind()
    };
    assert_eq!(fails(&|e| e.yaml_directive(2, 0)), ErrorKind::Emit, "not 1.x");
    assert_eq!(fails(&|e| e.tag_directive("e", "tag:a:")), ErrorKind::Emit, "not a handle");
    assert_eq!(fails(&|e| e.tag_directive("!e!", "")), ErrorKind::Emit, "an empty prefix");
    assert_eq!(fails(&|e| { e.tag_directive("!e!", "tag:a:")?; e.tag_directive("!e!", "tag:b:") }),
               ErrorKind::Emit, "a handle twice");
    assert_eq!(fails(&|e| { e.document_start(true)?; e.yaml_directive(1, 1) }), ErrorKind::Emit,
               "inside a document");
}

/// Each document's directives, as the parser gives them at its start.
fn directives_of(input: &str) -> Vec<Directives> {
    let parser = Parser::new(input).unwrap();
    let mut all = Vec::new();
    for event in parser.events() {
        if event.unwrap().kind == EventKind::DocumentStart {
            all.push(parser.directives());
        }
    }
    all
}

/// Each node's tag, in order.
fn tags_of(input: &str) -> Vec<Option<String>> {
    let parser = Parser::new(input).unwrap();
    let tags = parser
        .events()
        .map(|e| e.unwrap())
        .filter(|e| matches!(e.kind, EventKind::Scalar | EventKind::MappingStart | EventKind::SequenceStart))
        .map(|e| e.tag_str().map(|t| t.unwrap().to_owned()))
        .collect();
    tags
}

#[test]
fn directives_survive_reemitting() {
    let yaml = "%YAML 1.1\n%TAG !e! tag:example.com,2000:\n--- !e!x\na: !e!y yes\n...\n--- b\n";
    let docs = directives_of(yaml);
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[0].yaml_version, Some((1, 1)));
    assert_eq!(docs[0].tags, [(b"!e!".to_vec(), b"tag:example.com,2000:".to_vec())]);
    assert!(docs[1].is_empty(), "the second document has none");

    let mut e = Emitter::new().unwrap();
    e.reemit(&Parser::new(yaml).unwrap()).unwrap();
    let out = String::from_utf8(e.output()).unwrap();
    assert!(out.starts_with("%YAML 1.1\n%TAG !e! tag:example.com,2000:\n---"), "{out}");
    assert!(out.contains("!e!x") && out.contains("!e!y"), "tags written with the handle: {out}");
    assert_eq!(directives_of(&out), docs, "{out}");
    assert_eq!(tags_of(&out), tags_of(yaml), "{out}");
}

#[test]
fn directives_built_by_hand() {
    let mut e = Emitter::new().unwrap();
    e.stream_start().unwrap();
    e.directives(&Directives { yaml_version: None, tags: vec![(b"!e!".to_vec(), b"tag:a b:".to_vec())] }).unwrap();
    e.document_start(true).unwrap();
    e.scalar("v", ScalarStyle::Plain, None, Some("tag:a b:x")).unwrap();
    e.document_end(true).unwrap();
    e.stream_end().unwrap();
    let out = String::from_utf8(e.output()).unwrap();
    // the prefix's space is %-escaped; the document gets "---"
    assert_eq!(out, "%TAG !e! tag:a%20b:\n---\n!e!x v\n");
    assert_eq!(tags_of(&out), [Some("tag:a b:x".to_owned())]);
}

#[test]
fn parsers_and_emitters_are_send() {
    fn send<T: Send>() {}
    send::<Parser<'static>>();
    send::<Emitter>();
    send::<Schema>();
}
