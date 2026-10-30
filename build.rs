//! Embeds every file under `web/static/` (the UI) into the binary. New files are picked up
//! automatically; no list to maintain and no JS toolchain involved.
use std::{env, fs, path::Path};

fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            walk(&p, root, out);
        } else if let (Ok(rel), Ok(abs)) = (p.strip_prefix(root), fs::canonicalize(&p)) {
            let name = rel.to_string_lossy().replace('\\', "/");
            if !name.starts_with('.') && !name.contains("/.") {
                out.push((name, abs.to_string_lossy().into_owned()));
            }
        }
    }
}

fn main() {
    let root = Path::new("web/static");
    println!("cargo:rerun-if-changed=web/static");
    let mut files = Vec::new();
    walk(root, root, &mut files);
    let mut src = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for (name, abs) in &files {
        src.push_str(&format!("    ({name:?}, include_bytes!({abs:?})),\n"));
    }
    src.push_str("];\n");
    let out = Path::new(&env::var("OUT_DIR").expect("OUT_DIR")).join("static_files.rs");
    fs::write(out, src).expect("write static_files.rs");
}
