fn main() {
    let sdl = include_str!(concat!(env!("OUT_DIR"), "/schema.graphqls"));

    let path = std::env::args().nth(1);
    match path {
        Some(p) => {
            if let Some(parent) = std::path::Path::new(&p).parent() {
                if let Err(error) = std::fs::create_dir_all(parent) {
                    eprintln!("failed to create parent directories of {p}: {error}");
                    std::process::exit(1);
                }
            }
            if let Err(error) = std::fs::write(&p, sdl) {
                eprintln!("failed to write schema file {p}: {error}");
                std::process::exit(1);
            }
            eprintln!("Wrote GraphQL schema to {p}");
        },
        None => print!("{sdl}"),
    }
}
