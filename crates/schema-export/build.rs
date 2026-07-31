use std::{env, fs, path::PathBuf, sync::Arc};

fn main() {
    println!("cargo::rerun-if-changed=../graphql/src/");

    let services = Arc::new(chelix_service_traits::Services::default());
    let (tx, _rx) = tokio::sync::broadcast::channel(1);
    let schema = chelix_graphql::build_schema(services, tx);
    let sdl = schema.sdl();

    let out_dir = PathBuf::from(
        env::var("OUT_DIR").unwrap_or_else(|error| panic!("OUT_DIR not set: {error}")),
    );
    fs::write(out_dir.join("schema.graphqls"), sdl)
        .unwrap_or_else(|error| panic!("failed to write schema.graphqls: {error}"));
}
