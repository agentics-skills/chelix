//! Compile-time check for generated web assets.
//!
//! Missing generated assets fail the build. `include_dir!` would otherwise
//! embed an incomplete UI.

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let assets = std::path::Path::new(&manifest_dir).join("src/assets");

    let css_ok = assets.join("css/style.css").exists();
    let dist_ok = assets.join("dist/main.js").exists();
    let sw_ok = assets.join("sw.js").exists();

    if !css_ok || !dist_ok || !sw_ok {
        let mut lines: Vec<&str> = vec!["Web assets missing:"];
        if !css_ok {
            lines.push("  - css/style.css  ->  just build-css");
        }
        if !dist_ok {
            lines.push("  - dist/          ->  just build-frontend");
        }
        if !sw_ok {
            lines.push("  - sw.js          ->  just build-sw");
        }
        lines.push("Or build everything:  just build-web-assets");

        for line in &lines {
            println!("cargo:warning={line}");
        }
        std::process::exit(1);
    }

    println!("cargo:rerun-if-changed=src/assets/css/style.css");
    println!("cargo:rerun-if-changed=src/assets/dist/main.js");
    println!("cargo:rerun-if-changed=src/assets/sw.js");
}
