#![cfg(feature = "serde")]

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

use oyl::{Dialect, ErrorKind, Parser};
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
struct Config<'a> {
    name: &'a str,
    port: u16,
    debug: bool,
    ratio: f64,
    tags: Vec<String>,
    owner: Option<String>,
    limits: BTreeMap<String, u32>,
}

#[test]
fn a_struct() {
    let input = "name: web\nport: 8080\ndebug: false\nratio: 0.5\ntags: [a, b]\nowner: ~\nlimits: {cpu: 2, mem: 512}\n";
    let c: Config = oyl::from_str(input).unwrap();
    assert_eq!(
        c,
        Config {
            name: "web",
            port: 8080,
            debug: false,
            ratio: 0.5,
            tags: vec!["a".into(), "b".into()],
            owner: None,
            limits: [("cpu".to_string(), 2), ("mem".to_string(), 512)].into(),
        }
    );
    // the &str borrows from the input
    let range = input.as_bytes().as_ptr_range();
    assert!(range.contains(&c.name.as_ptr()));
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Deployment {
    api_version: String,
    kind: String,
    metadata: Metadata,
    spec: Spec,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Metadata {
    name: String,
    #[serde(default)]
    labels: HashMap<String, String>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Spec {
    replicas: u32,
    paused: bool,
    #[serde(default)]
    containers: Vec<Container>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Container {
    name: String,
    image: String,
    image_pull_policy: Option<String>,
}

const DEPLOYMENT: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  labels: &labels
    app: web
    tier: front
spec:
  replicas: 3
  paused: no
  containers:
    - &base
      name: nginx
      image: nginx:1.27
      imagePullPolicy: IfNotPresent
    - <<: *base
      name: sidecar
      image: envoy:1.30
";

#[test]
fn kubernetes_with_merges_by_go_yaml_v2() {
    let d: Deployment = oyl::from_str_with(DEPLOYMENT, Dialect::goyaml2()).unwrap();
    assert_eq!(d.api_version, "apps/v1");
    assert_eq!(d.metadata.labels["tier"], "front");
    assert_eq!(d.spec.replicas, 3);
    assert!(!d.spec.paused, "no is false to go-yaml v2");
    assert_eq!(d.spec.containers.len(), 2);
    let sidecar = &d.spec.containers[1];
    assert_eq!((sidecar.name.as_str(), sidecar.image.as_str()), ("sidecar", "envoy:1.30"));
    assert_eq!(sidecar.image_pull_policy.as_deref(), Some("IfNotPresent"), "merged from the alias");

    // YAML 1.2 reads `no` as text, which isn't a bool
    let e = oyl::from_str::<Deployment>(DEPLOYMENT).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Data);
    let m = e.mark().unwrap();
    assert_eq!((m.line, m.column), (10, 11), "{e}");
}

#[test]
fn errors_say_where() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct S {
        a: u8,
        b: Vec<u8>,
    }
    let e = oyl::from_str::<S>("a: 1\nb: [1, 2, three]\n").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Data);
    assert_eq!((e.mark().unwrap().line, e.mark().unwrap().column), (2, 11), "{e}");
    assert!(e.to_string().contains("expected u8"), "{e}");

    let e = oyl::from_str::<S>("a: 300\nb: []\n").unwrap_err();
    assert_eq!(e.mark().unwrap().line, 1, "{e}");

    // a missing field: where the mapping is
    let e = oyl::from_str::<S>("x: 1\na: 1\n").unwrap_err();
    assert!(e.to_string().contains("missing field `b`"), "{e}");
    assert_eq!(e.mark().unwrap().line, 1);

    // a syntax error comes from the parser, with its position
    let e = oyl::from_str::<S>("a: [1\n").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Parse);
}

#[test]
fn strings_from_any_scalar_but_null() {
    let m: HashMap<String, String> = oyl::from_str("version: 1.20\ncount: 007\nflag: true\nq: 'x'\n").unwrap();
    assert_eq!(m["version"], "1.20");
    assert_eq!(m["count"], "007");
    assert_eq!(m["flag"], "true");
    assert_eq!(m["q"], "x");
    assert!(oyl::from_str::<HashMap<String, String>>("a: ~\n").is_err());
    let m: HashMap<String, Option<String>> = oyl::from_str("a: ~\nb:\nc: 'null'\n").unwrap();
    assert_eq!((m["a"].as_deref(), m["b"].as_deref(), m["c"].as_deref()), (None, None, Some("null")));
}

#[test]
fn nothing_reads_as_null() {
    assert_eq!(oyl::from_str::<Option<u32>>("").unwrap(), None);
    oyl::from_str::<()>("").unwrap();
    oyl::from_str::<()>("~\n").unwrap();
    assert!(oyl::from_str::<u32>("").is_err());
}

#[test]
fn borrowing() {
    // a rewritten scalar (an escape) can't be borrowed from the input
    assert!(oyl::from_str::<&str>("\"a\\tb\"").is_err());
    assert_eq!(oyl::from_str::<String>("\"a\\tb\"").unwrap(), "a\tb");
    #[derive(Deserialize)]
    struct Text<'a> {
        #[serde(borrow)]
        t: Cow<'a, str>,
    }
    let plain: Text = oyl::from_str("t: plain").unwrap();
    let rewritten: Text = oyl::from_str("t: \"a\\tb\"").unwrap();
    assert!(matches!(plain.t, Cow::Borrowed("plain")));
    assert!(matches!(rewritten.t, Cow::Owned(ref s) if s == "a\tb"));
    // from a tree, everything borrows from the tree
    let parser = Parser::new("k: \"a\\tb\"\n").unwrap();
    let tree = parser.next_tree().unwrap().unwrap();
    let m: HashMap<&str, &str> = oyl::from_node(tree.root()).unwrap();
    assert_eq!(m["k"], "a\tb");
}

#[derive(Debug, Deserialize, PartialEq)]
enum Shape {
    Empty,
    Circle(f64),
    Rect { w: u32, h: u32 },
    Pair(i32, i32),
}

#[test]
fn enums() {
    let shapes: Vec<Shape> = oyl::from_str(
        "- Empty\n- Circle: 1.5\n- Rect: {w: 2, h: 3}\n- Pair: [1, -1]\n- !Circle 2.5\n- !Rect {w: 1, h: 1}\n- !Empty\n",
    )
    .unwrap();
    assert_eq!(
        shapes,
        [
            Shape::Empty,
            Shape::Circle(1.5),
            Shape::Rect { w: 2, h: 3 },
            Shape::Pair(1, -1),
            Shape::Circle(2.5),
            Shape::Rect { w: 1, h: 1 },
            Shape::Empty,
        ]
    );
    let e = oyl::from_str::<Shape>("Triangle").unwrap_err();
    assert!(e.to_string().contains("unknown variant"), "{e}");
    assert!(oyl::from_str::<Shape>("{Circle: 1, Empty: ~}").is_err());
}

#[test]
fn aliases_and_their_limits() {
    let v: Vec<Vec<i32>> = oyl::from_str("- &a [1, 2]\n- *a\n- *a\n").unwrap();
    assert_eq!(v, [[1, 2], [1, 2], [1, 2]]);

    // an alias inside its own anchor: an error, not a stack overflow
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Node {
        next: Option<Box<Node>>,
    }
    let e = oyl::from_str::<Node>("&a {next: *a}\n").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Limit, "{e}");

    // an alias bomb: an error, not a billion nodes
    let mut bomb = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]\n");
    for (i, name) in "bcdefghij".chars().enumerate() {
        let prev = (b'a' + i as u8) as char;
        bomb += &format!("{name}: &{name} [*{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}]\n");
    }
    let e = oyl::from_str::<serde::de::IgnoredAny>(&bomb);
    assert!(e.is_ok(), "ignoring it visits nothing");
    let e = oyl::from_str::<HashMap<String, serde_value::Any>>(&bomb).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Limit, "{e}");
}

// A self-describing value, as serde_json::Value would be, to visit everything
mod serde_value {
    use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

    #[derive(Debug)]
    pub struct Any;

    impl<'de> Deserialize<'de> for Any {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Any;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("anything")
                }
                fn visit_unit<E>(self) -> Result<Any, E> { Ok(Any) }
                fn visit_bool<E>(self, _: bool) -> Result<Any, E> { Ok(Any) }
                fn visit_i64<E>(self, _: i64) -> Result<Any, E> { Ok(Any) }
                fn visit_u64<E>(self, _: u64) -> Result<Any, E> { Ok(Any) }
                fn visit_f64<E>(self, _: f64) -> Result<Any, E> { Ok(Any) }
                fn visit_str<E>(self, _: &str) -> Result<Any, E> { Ok(Any) }
                fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Any, A::Error> {
                    while a.next_element::<Any>()?.is_some() {}
                    Ok(Any)
                }
                fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Any, A::Error> {
                    while a.next_entry::<Any, Any>()?.is_some() {}
                    Ok(Any)
                }
            }
            d.deserialize_any(V)
        }
    }
}

#[test]
fn nesting_limit() {
    let deep = format!("{}1{}", "[".repeat(200), "]".repeat(200));
    let e = oyl::from_str::<serde_value::Any>(&deep).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Limit, "{e}");
    let ok = format!("{}1{}", "[".repeat(100), "]".repeat(100));
    oyl::from_str::<serde_value::Any>(&ok).unwrap();
}

#[test]
fn keys_written_twice() {
    let e = oyl::from_str::<HashMap<String, i32>>("a: 1\nb: 2\na: 3\n").unwrap_err();
    assert_eq!(e.mark().unwrap().line, 3, "{e}");
    // a merge isn't a repeat: the keys written win (YAML), or the merge does (go-yaml v2)
    let input = "base: &b {x: 1, y: 2}\nm: {y: 9, <<: *b}\n";
    let m: HashMap<String, HashMap<String, i32>> = oyl::from_str(input).unwrap();
    assert_eq!(m["m"]["y"], 9);
    assert_eq!(m["m"]["x"], 1);
    let m: HashMap<String, HashMap<String, i32>> = oyl::from_str_with(input, Dialect::goyaml2()).unwrap();
    assert_eq!(m["m"]["y"], 2);
}

#[test]
fn documents() {
    let e = oyl::from_str::<i32>("1\n---\n2\n").unwrap_err();
    assert!(e.to_string().contains("more than one document"), "{e}");
    assert_eq!(e.mark().unwrap().line, 3, "the second document's root");
    let parser = Parser::new("--- {a: 1}\n--- {a: 2}\n").unwrap();
    let all: Vec<HashMap<String, i32>> = parser.trees().map(|t| oyl::from_node(t.unwrap().root()).unwrap()).collect();
    assert_eq!(all[1]["a"], 2);
    // part of a tree
    let tree = Parser::new("outer: {inner: [3, 4]}\n").unwrap().next_tree().unwrap().unwrap();
    let inner: Vec<u8> = oyl::from_node(tree.root().get("outer").unwrap().get("inner").unwrap()).unwrap();
    assert_eq!(inner, [3, 4]);
}

#[test]
fn readers_and_odd_types() {
    let v: Vec<u64> = oyl::from_reader(&b"[1, 18446744073709551615]"[..]).unwrap();
    assert_eq!(v, [1, u64::MAX]);
    let big: u128 = oyl::from_str("18446744073709551615").unwrap();
    assert_eq!(big, u64::MAX as u128);
    let t: (char, bool, f32) = oyl::from_str("[x, true, .inf]").unwrap();
    assert_eq!(t, ('x', true, f32::INFINITY));
    let f: f64 = oyl::from_str("3").unwrap();
    assert_eq!(f, 3.0);
    let ints: HashMap<i32, String> = oyl::from_str("1: one\n-2: minus two\n").unwrap();
    assert_eq!(ints[&-2], "minus two");
    assert!(oyl::from_str::<(i32, i32)>("[1, 2, 3]").is_err(), "an item left over");
    #[derive(Debug, Deserialize, PartialEq)]
    struct Unit;
    #[derive(Debug, Deserialize, PartialEq)]
    struct Meters(f64);
    assert_eq!(oyl::from_str::<Unit>("~").unwrap(), Unit);
    assert_eq!(oyl::from_str::<Meters>("2.5").unwrap(), Meters(2.5));
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Strict {
        a: i32,
    }
    assert!(oyl::from_str::<Strict>("a: 1\nb: 2\n").is_err());
}
