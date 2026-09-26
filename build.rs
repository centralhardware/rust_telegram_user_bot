//! Embeds `migrations/*.sql` into the binary, so the image carries its own
//! schema history and the migration runner never depends on files beside it.

use std::fmt::Write;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=migrations");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");

    // cargo-chef builds the dependencies from a skeleton that has no
    // migrations; an empty list is right for that pass.
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "sql"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();

    let mut out = String::from("pub static MIGRATIONS: &[(&str, &str)] = &[\n");
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy();
        writeln!(out, "    ({name:?}, include_str!({:?})),", path.display()).unwrap();
    }
    out.push_str("];\n");

    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("migrations.rs");
    std::fs::write(dest, out).unwrap();
}
