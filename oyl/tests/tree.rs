use oyl::{Dialect, Emitter, ErrorKind, JsonOptions, MergeOrder, NodeKind, Parser, ScalarStyle, Schema, Tree, Value};

fn tree_of(input: &str) -> Tree<'_> {
    Parser::new(input).unwrap().next_tree().unwrap().expect("a document")
}

fn texts<'t>(nodes: impl Iterator<Item = oyl::Node<'t>>) -> Vec<String> {
    nodes.map(|n| n.text_str().unwrap().to_owned()).collect()
}

#[test]
fn structure() {
    let input = "a: 1\nb:\n  - x\n  - [y, z]\nc: {d: e}\n";
    let tree = tree_of(input);
    let root = tree.root();
    assert_eq!(root.kind(), NodeKind::Mapping);
    assert_eq!(root.len(), 3);
    assert_eq!(tree.node_count(), tree.raw_nodes().len());
    let keys: Vec<_> = root.pairs().map(|(k, _)| k.text_str().unwrap()).collect();
    assert_eq!(keys, ["a", "b", "c"]);
    let b = root.get("b").unwrap();
    assert_eq!(b.kind(), NodeKind::Sequence);
    assert_eq!(b.items().len(), 2);
    let inner = b.at(1).unwrap();
    assert!(inner.is_flow());
    assert_eq!(texts(inner.items()), ["y", "z"]);
    assert_eq!(inner.parent(), Some(b));
    assert_eq!(root.parent(), None);
    assert!(root.pairs().next().unwrap().0.is_key());
    assert!(b.at(2).is_none() && root.get("zz").is_none());
    // a node's span is the input it was read from
    let d = root.get("c").unwrap();
    assert_eq!(&input[d.span()], "{d: e}");
    assert_eq!(&input[root.get("a").unwrap().span()], "1");
    assert_eq!(texts(tree.nodes().filter(|n| n.kind() == NodeKind::Scalar)).len(), 9);
}

#[test]
fn text_and_styles() {
    let tree = tree_of("q: \"a\\tb\"\ns: 'it''s'\nf: >\n  one\n  two\n");
    let root = tree.root();
    assert_eq!(root.get("q").unwrap().text(), b"a\tb");
    assert_eq!(root.get("q").unwrap().style(), ScalarStyle::DoubleQuoted);
    assert_eq!(root.get("s").unwrap().text(), b"it's");
    assert_eq!(root.get("f").unwrap().text(), b"one two\n");
    assert_eq!(root.get("f").unwrap().style(), ScalarStyle::Folded);
    assert_eq!(root.get("q").unwrap().mark().line, 1);
    assert_eq!(root.get("f").unwrap().mark().line, 3);
}

#[test]
fn values_by_dialect() {
    let input = "i: 42\nf: 1.5\nb: true\nn: ~\ns: hello\ny: yes\no: 0777\nbig: 18446744073709551615\n";
    let tree = tree_of(input);
    let v = |k: &str| tree.root().get(k).unwrap().value().unwrap();
    assert_eq!(tree.dialect(), Dialect::yaml12());
    assert_eq!(v("i"), Value::Int(42));
    assert_eq!(v("f"), Value::Float(1.5));
    assert_eq!(v("b"), Value::Bool(true));
    assert!(v("n").is_null());
    assert_eq!(v("s").as_str().unwrap().unwrap(), "hello");
    assert_eq!(v("y"), Value::Str(b"yes"));
    assert_eq!(v("o"), Value::Int(777));
    assert_eq!(v("big"), Value::Uint(u64::MAX));
    assert_eq!(v("big").as_i64(), None);
    assert_eq!(v("i").as_f64(), Some(42.0));

    let mut parser = Parser::new(input).unwrap();
    parser.dialect(Dialect::goyaml2());
    let tree = parser.next_tree().unwrap().unwrap();
    assert_eq!(tree.dialect(), Dialect::goyaml2());
    let v = |k: &str| tree.root().get(k).unwrap().value().unwrap();
    assert_eq!(v("y"), Value::Bool(true));
    assert_eq!(v("o"), Value::Int(0o777));
    assert_eq!(tree.root().get("y").unwrap().tag(), Some(&b"tag:yaml.org,2002:bool"[..]));

    // a document's %YAML 1.1 chooses YAML 1.1's types
    let tree = tree_of("%YAML 1.1\n---\non: off\n");
    assert_eq!(tree.dialect(), Dialect::yaml11());
    let (k, v) = tree.root().pairs().next().unwrap();
    assert_eq!(k.value().unwrap(), Value::Bool(true));
    assert_eq!(v.value().unwrap(), Value::Bool(false));
}

#[test]
fn value_errors() {
    let tree = tree_of("bad: !!int abc\nlist: [1]\n");
    let e = tree.root().get("bad").unwrap().value().unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Parse);
    assert_eq!(e.mark().unwrap().line, 1);
    assert!(e.message().contains("tag:yaml.org,2002:int"), "{e}");
    let e = tree.root().get("list").unwrap().value().unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Input);
}

#[test]
fn aliases() {
    let tree = tree_of("a: &x {k: v}\nb: *x\nc: &n 5\nd: *n\n");
    let root = tree.root();
    let b = root.get("b").unwrap();
    assert_eq!(b.kind(), NodeKind::Alias);
    assert_eq!(b.text(), b"x");
    assert_eq!(b.target(), root.get("a").unwrap());
    assert_eq!(root.get("a").unwrap().anchor(), Some(&b"x"[..]));
    assert_eq!(b.anchor(), None);
    assert_eq!(b.tag(), None);
    // collection methods and values go through the alias
    assert_eq!(b.get("k").unwrap().text(), b"v");
    assert_eq!(b.len(), 1);
    assert_eq!(b.pairs().count(), 1);
    assert_eq!(root.get("d").unwrap().value().unwrap(), Value::Int(5));
}

const MERGES: &str = "base: &b {x: 1, y: 2}\nm:\n  y: 9\n  <<: *b\n  z: 3\n";

#[test]
fn merges_in_each_dialects_order() {
    let tree = tree_of(MERGES);
    assert_eq!(tree.dialect().merge_order(), MergeOrder::Yaml);
    let m = tree.root().get("m").unwrap();
    assert_eq!(m.get("x").unwrap().text(), b"1");
    assert_eq!(m.get("y").unwrap().text(), b"9", "YAML: keys written in the mapping win");
    let entries: Vec<_> = m.entries().unwrap().iter().map(|(k, v)| (k.text_str().unwrap(), v.text_str().unwrap())).collect();
    assert_eq!(entries, [("y", "9"), ("x", "1"), ("z", "3")]);
    assert_eq!(m.pairs().len(), 3, "pairs are as written, << included");

    let mut parser = Parser::new(MERGES).unwrap();
    parser.dialect(Dialect::goyaml2());
    let tree = parser.next_tree().unwrap().unwrap();
    assert_eq!(tree.dialect().merge_order(), MergeOrder::Ordered);
    let m = tree.root().get("m").unwrap();
    assert_eq!(m.get("y").unwrap().text(), b"2", "go-yaml v2: a merge overrides the keys before it");
}

#[test]
fn merges_as_pyyaml_does_them() {
    // a later << wins over an earlier one; an alias to a sequence merges as the sequence
    let input = "%YAML 1.1\n---\na: &a {x: a, y: a}\nb: &b {x: b, z: b}\nl: &l [*a, *b]\n\
                 two:\n  <<: *a\n  <<: *b\nlist:\n  <<: *l\n";
    let tree = tree_of(input);
    let two = tree.root().get("two").unwrap();
    assert_eq!(two.get("x").unwrap().text(), b"b");
    assert_eq!(two.get("y").unwrap().text(), b"a");
    let list = tree.root().get("list").unwrap();
    let entries: Vec<_> = list.entries().unwrap().iter().map(|(k, v)| (k.text_str().unwrap(), v.text_str().unwrap())).collect();
    assert_eq!(entries, [("x", "a"), ("y", "a"), ("z", "b")]);
}

#[test]
fn strict_entries_name_the_repeated_key() {
    let tree = tree_of("a: 1\nb: 2\na: 3\n");
    assert_eq!(tree.root().get("a").unwrap().text(), b"3");
    assert_eq!(tree.root().entries().unwrap().len(), 2);
    let e = tree.root().entries_strict().unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Parse);
    assert_eq!(e.mark().unwrap().line, 3);
    assert!(e.message().contains("'a'"), "{e}");
    assert_eq!(tree.root().get("a").unwrap().entries().unwrap_err().kind(), ErrorKind::Input);
}

#[test]
fn documents() {
    let input = "a: 1\n---\nb: 2\n...\n--- c\n";
    let parser = Parser::new(input).unwrap();
    let trees: Vec<Tree> = parser.trees().map(|t| t.unwrap()).collect();
    assert_eq!(trees.len(), 3);
    assert_eq!(trees[1].base(), 5);
    assert_eq!(trees[1].root().get("b").unwrap().span(), 12..13);
    assert_eq!(trees[2].root().text(), b"c");
    assert!(parser.next_tree().unwrap().is_none());
    // an empty document is an empty scalar
    let tree = tree_of("---\n...\n");
    assert_eq!(tree.node_count(), 1);
    assert_eq!(tree.root().value().unwrap(), Value::Null);
}

#[test]
fn a_tree_outlives_its_parser() {
    let input = String::from("k: \"rewritten\\n\"\n");
    let tree = {
        let parser = Parser::new(&input).unwrap();
        parser.next_tree().unwrap().unwrap()
    };
    assert_eq!(tree.root().get("k").unwrap().text(), b"rewritten\n");
}

#[test]
fn a_tree_outlives_a_parser_with_a_schema() {
    // the parser makes a dialect of its own from the schema; the tree keeps a copy
    let input = String::from("n: 42\n");
    let tree = {
        let mut parser = Parser::new(&input).unwrap();
        parser.schema(Schema::core());
        parser.next_tree().unwrap().unwrap()
    };
    assert_eq!(tree.dialect().merge_order(), MergeOrder::Yaml);
    assert_eq!(format!("{:?}", tree.dialect()), "Dialect(custom)");
    assert_eq!(tree.root().get("n").unwrap().value().unwrap(), Value::Int(42));
}

#[test]
fn tree_errors() {
    let parser = Parser::new("a: *nope\n").unwrap();
    let e = parser.next_tree().unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Parse);
    assert!(e.message().contains("nope"), "{e}");
    assert!(parser.next_tree().unwrap().is_none(), "nothing after an error");

    let parser = Parser::new("a: [1, 2\n").unwrap();
    assert_eq!(parser.trees().filter(Result::is_err).count(), 1);

    let parser = Parser::new("a: 1\n").unwrap();
    parser.next_event().unwrap();
    parser.next_event().unwrap();
    parser.next_event().unwrap(); // inside the document
    assert_eq!(parser.next_tree().unwrap_err().kind(), ErrorKind::Input);
}

#[test]
fn emitting_a_tree_writes_what_its_events_write() {
    let input = "--- &r\n? [a, b]\n: |\n  text\nk: !t {x: 'y'}\nl: *r\n...\n";
    let mut by_events = Emitter::new().unwrap();
    for e in Parser::new(input).unwrap().events() {
        by_events.event(&e.unwrap()).unwrap();
    }
    let mut by_tree = Emitter::new().unwrap();
    by_tree.stream_start().unwrap();
    for t in Parser::new(input).unwrap().trees() {
        by_tree.tree(&t.unwrap()).unwrap();
    }
    by_tree.stream_end().unwrap();
    assert_eq!(String::from_utf8(by_tree.output()).unwrap(), String::from_utf8(by_events.output()).unwrap());
}

#[test]
fn json() {
    let input = "b: yes\na: 0777\n<<: {c: 1}\n---\n- x\n";
    let parser = Parser::new(input).unwrap();
    let k8s = JsonOptions::kubernetes();
    assert_eq!(parser.next_json(&k8s).unwrap(), Some(r#"{"a":511,"b":true,"c":1}"#));
    assert_eq!(parser.next_json(&k8s).unwrap(), Some(r#"["x"]"#));
    assert_eq!(parser.next_json(&k8s).unwrap(), None);

    let parser = Parser::new("b: yes\n").unwrap();
    let core = JsonOptions { schema: Some(Schema::core()), ..JsonOptions::default() };
    assert_eq!(parser.next_json(&core).unwrap(), Some(r#"{"b":"yes"}"#));

    let parser = Parser::new("a: 1\na: 2\n").unwrap();
    let strict = JsonOptions { strict: true, ..JsonOptions::kubernetes() };
    let e = parser.next_json(&strict).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Parse);
    assert_eq!(e.mark().unwrap().line, 2);
}

#[test]
fn trees_are_shared_between_threads() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Tree<'static>>();
    send_sync::<Dialect<'static>>();
    let tree = tree_of("a: [1, 2, 3]\n");
    let sums: Vec<i64> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..3)
            .map(|_| s.spawn(|| tree.root().get("a").unwrap().items().map(|n| n.value().unwrap().as_i64().unwrap()).sum()))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(sums, [6, 6, 6]);
}

#[test]
fn dialects() {
    assert_eq!(Dialect::goyaml2(), Dialect::goyaml2());
    assert_ne!(Dialect::yaml12(), Dialect::goyaml2());
    assert_eq!(format!("{:?}", Dialect::yaml11()), "Dialect(yaml11)");
    assert_eq!(Dialect::goyaml2().schema().resolve(b"yes", ScalarStyle::Plain), b"tag:yaml.org,2002:bool");
    assert_eq!(Schema::yaml11().resolve(b"0b101", ScalarStyle::Plain), b"tag:yaml.org,2002:int");
    assert_eq!(Dialect::yaml12().version(), (1, 2));
    assert_eq!(Dialect::yaml11().version(), (1, 1));
    assert_eq!(Dialect::goyaml2().version(), (1, 1));
}

#[test]
fn a_documents_version_chooses_its_dialect() {
    assert_eq!(tree_of("%YAML 1.1\n---\na: yes\n").dialect(), Dialect::yaml11());
    assert_eq!(tree_of("a: yes\n").dialect(), Dialect::yaml12());
    let mut parser = Parser::new("%YAML 1.2\n---\na: yes\n").unwrap();
    parser.dialect(Dialect::yaml11());
    assert_eq!(parser.next_tree().unwrap().unwrap().dialect(), Dialect::yaml12());
    // go-yaml v2's dialect keeps a document of its version, and refuses others
    let mut parser = Parser::new("%YAML 1.1\n---\na: yes\n").unwrap();
    parser.dialect(Dialect::goyaml2());
    assert_eq!(parser.next_tree().unwrap().unwrap().dialect(), Dialect::goyaml2());
    let mut parser = Parser::new("%YAML 1.2\n---\na: yes\n").unwrap();
    parser.dialect(Dialect::goyaml2());
    assert_eq!(parser.next_tree().unwrap_err().kind(), ErrorKind::Parse);
}

#[test]
fn a_tree_keeps_its_directives() {
    let yaml = "%YAML 1.1\n%TAG !e! tag:example.com,2000:\n--- !e!x\na: !e!y yes\n";
    let tree = tree_of(yaml);
    let d = tree.directives();
    assert_eq!(d.yaml_version, Some((1, 1)));
    assert_eq!(d.tags, [(b"!e!".to_vec(), b"tag:example.com,2000:".to_vec())]);
    assert!(tree_of("a: 1\n").directives().is_empty());

    let mut e = Emitter::new().unwrap();
    e.stream_start().unwrap();
    e.tree(&tree).unwrap();
    e.stream_end().unwrap();
    let out = String::from_utf8(e.output()).unwrap();
    assert!(out.starts_with("%YAML 1.1\n%TAG !e! tag:example.com,2000:\n---"), "{out}");
    let again = tree_of(&out);
    assert_eq!(again.directives(), d, "{out}");
    assert_eq!(again.root().tag(), Some(&b"tag:example.com,2000:x"[..]));
    assert_eq!(again.root().get("a").unwrap().value().unwrap(), Value::Str(b"yes"), "a local type keeps its text");

    // a %TAG given first replaces the tree's for its handle; its %YAML stays
    let mut e = Emitter::new().unwrap();
    e.stream_start().unwrap();
    e.tag_directive("!e!", "tag:other.org,2000:").unwrap();
    e.tree(&tree).unwrap();
    e.stream_end().unwrap();
    let out = String::from_utf8(e.output()).unwrap();
    assert!(out.starts_with("%YAML 1.1\n%TAG !e! tag:other.org,2000:\n---"), "{out}");
    assert_eq!(tree_of(&out).root().tag(), Some(&b"tag:example.com,2000:x"[..]), "{out}");
}

#[test]
fn raw_arrays_hold_what_the_calls_give() {
    let tree = tree_of("%TAG !e! tag:x:\n--- &r\na: !e!t \"q\\tr\"\nb: &k [1, *k]\n");
    let (nodes, props, pool) = (tree.raw_nodes(), tree.raw_props(), tree.raw_pool());
    assert_eq!(props[0], oyl::sys::oyl_node_props::default(), "entry 0 is none");
    let text = |at: u32, len: u32| &pool[at as usize..(at + len) as usize];
    let mut tags = 0;
    for (i, n) in nodes.iter().enumerate() {
        let node = tree.node(i).unwrap();
        if node.kind() == NodeKind::Alias {
            assert_eq!(n.props as usize, node.target().index(), "an alias's props is its target");
            continue;
        }
        let p = props[n.props as usize];
        assert_eq!((p.anchor_len > 0).then(|| text(p.anchor, p.anchor_len)), node.anchor(), "node {i}");
        if p.tag_len > 0 {
            assert_eq!(Some(text(p.tag, p.tag_len)), node.tag(), "node {i}");
            tags += 1;
        }
        if n.flags & oyl::sys::OYL_NODE_POOLED != 0 {
            assert_eq!(text(n.text, n.len), node.text(), "a rewritten scalar's text is in the pool");
            assert_eq!(node.text(), b"q\tr");
        }
    }
    assert_eq!(tags, 1);
    assert_eq!(tree.root().get("a").unwrap().tag(), Some(&b"tag:x:t"[..]));
}

#[test]
fn merges_too_costly_to_work_out() {
    // each mapping merges the one before and adds a key: 12 million pairs
    let mut yaml = String::from("m0: &m0 {k0: v}\n");
    for i in 1..=5000 {
        yaml += &format!("m{i}: &m{i} {{<<: *m{}, k{i}: v}}\n", i - 1);
    }
    let mut parser = Parser::new(&yaml).unwrap();
    parser.max_events(0);
    let tree = parser.next_tree().unwrap().unwrap();
    let last = tree.root().get("m5000").unwrap();
    assert_eq!(last.entries().unwrap_err().kind(), ErrorKind::Limit);
    assert_eq!(tree.root().get("m0").unwrap().entries().unwrap().len(), 1, "one that doesn't merge");
    assert_eq!(last.get("k1").unwrap().text(), b"v", "a lookup walks the merges");
    assert!(last.get("nope").is_none());
}

#[test]
fn keys_compare_by_tag_and_value() {
    let tree = tree_of("1: int\n\"1\": str\n0x2: a\n2: b\ntrue: c\nTrue: d\n");
    let root = tree.root();
    let keys: Vec<_> = root.entries().unwrap().iter().map(|(k, v)| (k.text_str().unwrap(), v.text_str().unwrap())).collect();
    assert_eq!(keys, [("1", "int"), ("1", "str"), ("2", "b"), ("True", "d")]);
    // by text, the string first; by value, as YAML compares keys
    assert_eq!(root.get("1").unwrap().text(), b"str");
    assert_eq!(root.get("0x2").unwrap().text(), b"b");
    assert_eq!(root.get_value(Value::Int(1)).unwrap().text(), b"int");
    assert_eq!(root.get_value(Value::Str(b"1")).unwrap().text(), b"str");
    assert_eq!(root.get_value(Value::Bool(true)).unwrap().text(), b"d");
    assert!(root.get_value(Value::Null).is_none());
    // 0x2 and 2 repeat, as do true and True; 1 and "1" don't
    let e = root.entries_strict().unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Parse);
    assert_eq!(e.mark().unwrap().line, 4);
    assert!(tree_of("1: a\n\"1\": b\n").root().entries_strict().is_ok());

    // YAML 1.1's types make more keys one
    let mut parser = Parser::new("yes: a\non: b\ny: c\n").unwrap();
    parser.dialect(Dialect::yaml11());
    let tree = parser.next_tree().unwrap().unwrap();
    assert_eq!(tree.root().entries().unwrap().len(), 2);
    assert_eq!(tree.root().get("yes").unwrap().text(), b"b");
    assert_eq!(tree.root().get_value(Value::Bool(true)).unwrap().text(), b"b");
}
