//! Embeds the bundled qBittorrent WebUI (`webui/www/{public,private}`).
//!
//! Every file is pre-translated (English source text for each
//! `QBT_TR(...)QBT_TR[CONTEXT=...]` marker, like qBittorrent does at serve time
//! when no translation is loaded) and gzip-compressed, so the binary carries
//! ~1/4 of the raw size and most files can be sent compressed as is. The
//! generated table is included by `src/webui/assets.rs`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
#[path = "src/webui/translate.rs"]
mod translate;

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let www = manifest.join("webui").join("www");
    println!("cargo:rerun-if-changed=webui/www");
    println!("cargo:rerun-if-changed=src/webui/translate.rs");

    let mut files = Vec::new();
    for sub in ["public", "private"] {
        collect(&www.join(sub), &www, &mut files);
    }
    files.sort();

    let gz_dir = out.join("www");
    std::fs::create_dir_all(&gz_dir).expect("create OUT_DIR/www");

    let mut table = String::from("pub static ASSETS: &[Asset] = &[\n");
    for (i, rel) in files.iter().enumerate() {
        let raw = std::fs::read(www.join(rel)).expect("read WebUI asset");
        let (data, dynamic) = if translate::is_translatable(rel) {
            match String::from_utf8(raw) {
                Ok(text) => {
                    let translated = translate::translate_placeholders(&text);
                    let dynamic = translate::has_dynamic_placeholders(&translated);
                    (translated.into_bytes(), dynamic)
                }
                Err(e) => (e.into_bytes(), false),
            }
        } else {
            (raw, false)
        };
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&data).expect("gzip");
        let gz = enc.finish().expect("gzip");
        let gz_name = format!("{i}.gz");
        std::fs::write(gz_dir.join(&gz_name), gz).expect("write compressed asset");
        writeln!(
            table,
            "    Asset {{ path: {rel:?}, gz: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/www/{gz_name}\")), len: {}, dynamic: {dynamic} }},",
            data.len()
        )
        .expect("format");
    }
    table.push_str("];\n");
    std::fs::write(out.join("webui_assets.rs"), table).expect("write asset table");
}

fn collect(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            collect(&path, root, out);
        } else if ft.is_file() {
            let rel = path.strip_prefix(root).expect("asset under root");
            let rel = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            out.push(rel);
        }
    }
}
