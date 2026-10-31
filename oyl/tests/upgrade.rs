use oyl::{Dialect, FixKind, Upgrade};

#[test]
fn pyyaml_reading_kept_in_yaml_12_spelling() {
    let input = "%YAML 1.1\n---\non: push  # mine\nenabled: yes\nmode: 0755\nratio: 1e3\n";
    let (text, fixes) = oyl::upgrade(input, &Upgrade::from(Dialect::yaml11())).unwrap();
    assert_eq!(text, "---\n\"on\": push  # mine\nenabled: true\nmode: !!int 0o755\nratio: \"1e3\"\n");
    let kinds: Vec<FixKind> = fixes.iter().map(|f| f.kind).collect();
    assert_eq!(kinds, [FixKind::Directive, FixKind::Scalar, FixKind::Scalar, FixKind::Scalar, FixKind::Scalar]);
    let on = &fixes[1];
    assert_eq!((on.mark.line, on.mark.column, on.before.as_str(), on.after.as_str()), (3, 1, "on", "\"on\""));
    assert!(on.fixed && on.message.contains("the text kept"));
}

#[test]
fn go_yaml_v2_reading() {
    let (text, _) = oyl::upgrade("a: y\nb: 2001-12-14\n", &Upgrade::from(Dialect::goyaml2())).unwrap();
    assert_eq!(text, "a: true\nb: 2001-12-14\n");
}

#[test]
fn reader_unknown_reports_disagreements() {
    let (text, fixes) = oyl::upgrade("a: yes\nb: 1e3\n", &Upgrade::default()).unwrap();
    assert_eq!(text, "a: true\nb: 1e3\n");
    assert_eq!(fixes.len(), 2);
    assert!(!fixes[1].fixed && fixes[1].after.is_empty() && fixes[1].message.contains("name the dialect"));
}

#[test]
fn policy_flipped() {
    let options = Upgrade::from(Dialect::yaml11()).keys_keep_value(true).values_keep_text(true);
    let (text, _) = oyl::upgrade("on: 1\na: yes\n", &options).unwrap();
    assert_eq!(text, "true: 1\na: \"yes\"\n");
}

#[test]
fn unreadable_input_is_an_error() {
    let err = oyl::upgrade("a: [1\n", &Upgrade::from(Dialect::yaml11())).unwrap_err();
    assert!(err.mark().is_some());
}
