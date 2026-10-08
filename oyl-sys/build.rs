// Builds Oyl from the C sources vendored in oyl/ (see scripts/sync-oyl.sh)
// into a static library, with the flags Oyl's own Makefile uses.

use std::path::Path;

fn main() {
    let oyl = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("oyl");
    let include = oyl.join("include");
    let mut build = cc::Build::new();
    build
        .include(&include)
        .flag_if_supported("-std=c11")
        .flag_if_supported("-fvisibility=hidden")
        .warnings(false);
    let mut sources: Vec<_> = std::fs::read_dir(oyl.join("src"))
        .expect("oyl/src: the vendored C sources")
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "c"))
        .collect();
    sources.sort();
    for source in &sources {
        build.file(source);
    }
    // sizes and offsets of the structs src/lib.rs mirrors, for its layout test
    build.file("src/layout.c");
    build.compile("oyl");

    println!("cargo:rerun-if-changed=oyl");
    println!("cargo:rerun-if-changed=src/layout.c");
    // for crates that compile C against Oyl: DEP_OYL_INCLUDE
    println!("cargo:include={}", include.display());
}
