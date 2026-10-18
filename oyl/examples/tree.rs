//! Print each document of a YAML file as a tree: every node indented under
//! its collection, with its tag and, for scalars, its typed value.
//!
//!     cargo run --example tree -- file.yaml [--goyaml2]

use oyl::{Dialect, Node, NodeKind, Parser};

fn print(node: Node<'_>, depth: usize, label: &str) -> Result<(), Box<dyn std::error::Error>> {
    let pad = "  ".repeat(depth);
    let tag = node.tag().map(|t| format!(" <{}>", String::from_utf8_lossy(t))).unwrap_or_default();
    let anchor = node.anchor().map(|a| format!(" &{}", String::from_utf8_lossy(a))).unwrap_or_default();
    match node.kind() {
        NodeKind::Scalar => println!("{pad}{label}{:?}{anchor}{tag}", node.value()?),
        NodeKind::Alias => println!("{pad}{label}*{}", node.text_str()?),
        NodeKind::Sequence => {
            println!("{pad}{label}sequence{anchor}{tag}");
            for item in node.items() {
                print(item, depth + 1, "- ")?;
            }
        }
        NodeKind::Mapping => {
            println!("{pad}{label}mapping{anchor}{tag}");
            for (key, value) in node.pairs() {
                print(key, depth + 1, "? ")?;
                print(value, depth + 1, ": ")?;
            }
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.iter().find(|a| !a.starts_with("--")).ok_or("usage: tree FILE [--goyaml2]")?;
    let input = std::fs::read(path)?;
    let mut parser = Parser::new(&input)?;
    if args.iter().any(|a| a == "--goyaml2") {
        parser.dialect(Dialect::goyaml2());
    }
    for tree in parser.trees() {
        let tree = tree?;
        println!("--- {:?}, {} nodes", tree.dialect(), tree.node_count());
        print(tree.root(), 0, "")?;
    }
    Ok(())
}
