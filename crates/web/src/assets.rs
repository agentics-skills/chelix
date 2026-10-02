//! Static assets embedded in the binary.
//!
//! The only source is `include_dir!`. A missing file is a 404.

use std::{
    hash::Hasher,
    path::{Component, Path as FsPath},
    sync::LazyLock,
};

use {
    axum::{extract::Path, http::StatusCode, response::IntoResponse},
    serde::Serialize,
    tracing::info,
};

static ASSETS: include_dir::Dir<'_> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/src/assets");

struct AssetState {
    hash: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AssetVersionInfo<'a> {
    chelix_version: &'static str,
    asset_hash: &'a str,
    asset_source: &'static str,
}

impl AssetState {
    fn version_info(&self) -> AssetVersionInfo<'_> {
        AssetVersionInfo {
            chelix_version: chelix_config::VERSION,
            asset_hash: &self.hash,
            asset_source: "embedded",
        }
    }
}

static ASSET_STATE: LazyLock<AssetState> = LazyLock::new(|| {
    info!("Serving assets from embedded binary");
    AssetState {
        hash: hash_embedded_assets(),
    }
});

/// Content hash of the embedded assets, used for cache-busting URLs.
pub(crate) fn asset_content_hash() -> String {
    ASSET_STATE.hash.clone()
}

fn hash_embedded_assets() -> String {
    let mut files = std::collections::BTreeMap::new();
    let mut stack: Vec<&include_dir::Dir<'_>> = vec![&ASSETS];
    while let Some(dir) = stack.pop() {
        for file in dir.files() {
            files.insert(file.path().display().to_string(), file.contents());
        }
        for sub in dir.dirs() {
            stack.push(sub);
        }
    }
    hash_file_map(files.iter().map(|(path, bytes)| (path.as_str(), *bytes)))
}

fn hash_file_map<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> String {
    let mut hasher = std::hash::DefaultHasher::new();
    for (path, contents) in files {
        hasher.write(path.as_bytes());
        hasher.write(contents);
    }
    format!("{:016x}", hasher.finish())
}

fn mime_for_path(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "mjs" => "application/javascript; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" => "application/json",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => "application/octet-stream",
    }
}

fn embedded_path_allowed(path: &str) -> bool {
    let rel = FsPath::new(path);
    !rel.is_absolute()
        && rel
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn read_asset(path: &str) -> Option<Vec<u8>> {
    if path == "version.json" {
        return serde_json::to_vec(&ASSET_STATE.version_info()).ok();
    }
    if !embedded_path_allowed(path) {
        return None;
    }
    ASSETS.get_file(path).map(|file| file.contents().to_vec())
}

/// Read raw embedded asset bytes by path. Used by `share_render.rs` for the favicon.
pub fn read_asset_bytes(path: &str) -> Option<Vec<u8>> {
    read_asset(path)
}

/// Versioned assets: `/assets/v/<hash>/path` — immutable, cached forever.
pub async fn versioned_asset_handler(
    Path((_version, path)): Path<(String, String)>,
) -> impl IntoResponse {
    serve_asset(&path, "public, max-age=31536000, immutable")
}

/// Unversioned assets: `/assets/path` — always revalidate.
pub async fn asset_handler(Path(path): Path<String>) -> impl IntoResponse {
    serve_asset(&path, "no-cache")
}

/// Canonical browser favicon: `/favicon.ico`.
pub async fn favicon_handler() -> impl IntoResponse {
    serve_asset("icons/favicon-32.png", "no-cache")
}

/// PWA manifest: `/manifest.json`.
pub async fn manifest_handler() -> impl IntoResponse {
    serve_asset("manifest.json", "no-cache")
}

/// Service worker: `/sw.js`.
pub async fn service_worker_handler() -> impl IntoResponse {
    serve_asset("sw.js", "no-cache")
}

fn serve_asset(path: &str, cache_control: &'static str) -> axum::response::Response {
    match read_asset(path) {
        Some(body) => {
            let mut response = (
                StatusCode::OK,
                [
                    ("content-type", mime_for_path(path)),
                    ("cache-control", cache_control),
                    ("x-content-type-options", "nosniff"),
                ],
                body,
            )
                .into_response();
            if path.rsplit('.').next().unwrap_or("") == "svg" {
                response.headers_mut().insert(
                    axum::http::header::CONTENT_SECURITY_POLICY,
                    axum::http::HeaderValue::from_static(
                        "default-src 'none'; img-src 'self' data:; style-src 'none'; script-src 'none'; object-src 'none'; frame-ancestors 'none'",
                    ),
                );
            }
            response
        },
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{ASSET_STATE, hash_embedded_assets};

    #[test]
    fn version_json_reports_embedded_source_and_hash() {
        let version_json = serde_json::to_value(ASSET_STATE.version_info())
            .unwrap_or_else(|error| panic!("json failed: {error}"));
        assert_eq!(version_json["assetSource"], "embedded");
        assert!(version_json.get("fallbackReason").is_none());
        assert_eq!(version_json["assetHash"], hash_embedded_assets());
        assert_eq!(version_json["chelixVersion"], chelix_config::VERSION);
    }
}
