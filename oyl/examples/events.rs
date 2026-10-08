//! Print a YAML file's events, one per line, like libyaml's `run-parser`.
//!
//!     cargo run --example events -- file.yaml [--merge] [--resolve]

use oyl::{EventKind, Parser};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.iter().find(|a| !a.starts_with("--")).ok_or("usage: events FILE [--merge] [--resolve]")?;
    let input = std::fs::read(path)?;
    let mut parser = Parser::new(&input)?;
    parser.merge(args.iter().any(|a| a == "--merge")).resolve(args.iter().any(|a| a == "--resolve"));
    for event in parser.events() {
        let e = event?;
        let mut line = format!("{}:{} {:?}", e.start.line, e.start.column, e.kind);
        if let Some(anchor) = e.anchor {
            line += &format!(" &{}", String::from_utf8_lossy(anchor));
        }
        if let Some(tag) = e.tag {
            line += &format!(" <{}>", String::from_utf8_lossy(tag));
        }
        if matches!(e.kind, EventKind::Scalar | EventKind::Alias) {
            line += &format!(" {:?}", String::from_utf8_lossy(e.value));
        }
        println!("{line}");
    }
    Ok(())
}
