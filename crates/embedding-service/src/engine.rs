use std::path::{Path, PathBuf};

use {
    anyhow::{Context, Result, bail},
    async_trait::async_trait,
    chelix_embedding_service::{EmbeddingEngine, pool::mean_pool_normalized_windows},
    chelix_protocol::EmbeddingModelMetadata,
    either::Either,
    mistralrs::{
        AutoDeviceMapParams, DeviceMapSetting, EmbeddingModelBuilder, EmbeddingRequest, IsqType,
        TokenSource, UqffEmbeddingModelBuilder,
    },
    serde::Deserialize,
    sha2::{Digest, Sha256},
};

const UQFF_DIR_NAME: &str = "chelix-uqff";
const UQFF_STEM: &str = "chelix-q8_0";
const IDENTITY_FILE: &str = "chelix-identity";
const RESIDUAL_FILE: &str = "residual.safetensors";

pub(crate) struct ModelLoadArgs {
    pub model_spec: String,
    pub hf_token: Option<String>,
    pub cache_dir: Option<PathBuf>,
}

struct LoadedModel {
    model: mistralrs::Model,
    identity: String,
}

pub(crate) struct LocalMistralEngine {
    model: mistralrs::Model,
    metadata: EmbeddingModelMetadata,
    max_tokens: usize,
}

impl LocalMistralEngine {
    pub(crate) async fn new(args: ModelLoadArgs) -> Result<Self> {
        refuse_gguf(&args.model_spec)?;
        let model_spec = args.model_spec.clone();
        let loaded = tokio::task::spawn_blocking(move || load_q8_model(args))
            .await
            .map_err(|error| anyhow::anyhow!("joining model load: {error}"))??;

        let max_tokens = loaded
            .model
            .max_sequence_length()
            .map_err(|error| anyhow::anyhow!("failed to read model context length: {error}"))?
            .ok_or_else(|| anyhow::anyhow!("model did not report a maximum sequence length"))?;
        if max_tokens == 0 {
            bail!("model reported zero maximum sequence length");
        }

        let probe = embed_prompt(&loaded.model, "probe")
            .await
            .map_err(|error| anyhow::anyhow!("failed to probe embedding dimensions: {error}"))?;
        let dimensions = probe.len();
        if dimensions == 0 {
            bail!("model reported zero embedding dimensions");
        }
        if probe.iter().any(|value| !value.is_finite()) {
            bail!("probe embedding contained non-finite values");
        }
        if probe.iter().all(|value| *value == 0.0) {
            bail!("probe embedding was all zeros");
        }

        let metadata = EmbeddingModelMetadata {
            model_name: loaded.identity.clone(),
            dimensions,
            provider_key: loaded.identity,
        };

        #[cfg(feature = "tracing")]
        tracing::info!(
            model_spec,
            model = %metadata.model_name,
            dimensions,
            max_tokens,
            "loaded local mistral.rs embedding model"
        );

        #[cfg(not(feature = "tracing"))]
        let _ = model_spec;
        Ok(Self {
            model: loaded.model,
            metadata,
            max_tokens,
        })
    }
}

#[async_trait]
impl EmbeddingEngine for LocalMistralEngine {
    async fn embed(&self, text: &str, _priority: u32) -> Result<Vec<f32>> {
        let tokens = self
            .model
            .tokenize(Either::Right(text.to_owned()), None, true, false, None)
            .await
            .map_err(|error| anyhow::anyhow!("tokenization failed: {error}"))?;
        if tokens.is_empty() {
            bail!("empty token sequence");
        }

        if tokens.len() <= self.max_tokens {
            return embed_prompt(&self.model, text).await;
        }

        #[cfg(feature = "tracing")]
        tracing::warn!(
            token_count = tokens.len(),
            max_tokens = self.max_tokens,
            window_count = tokens.len().div_ceil(self.max_tokens),
            "local embedding input exceeds model context; pooling windows"
        );

        let mut windows = Vec::new();
        for window in tokens.chunks(self.max_tokens) {
            windows.push(embed_tokens(&self.model, window, false).await?);
        }
        mean_pool_normalized_windows(&windows)
    }

    fn metadata(&self) -> &EmbeddingModelMetadata {
        &self.metadata
    }
}

fn cpu_device_map() -> DeviceMapSetting {
    DeviceMapSetting::Auto(AutoDeviceMapParams::Text {
        max_seq_len: 2048,
        max_batch_size: 1,
    })
}

fn artifact_key(model_spec: &str) -> String {
    sha256_hex(model_spec.as_bytes())
}

fn artifact_dir(model_spec: &str, cache_dir: Option<&Path>) -> PathBuf {
    if let Some(cache) = cache_dir {
        return cache.join("uqff").join(artifact_key(model_spec));
    }
    let spec_path = Path::new(model_spec);
    if spec_path.is_dir() {
        return spec_path.join(UQFF_DIR_NAME);
    }
    PathBuf::from(".chelix/models")
        .join("uqff")
        .join(artifact_key(model_spec))
}

fn staging_dir(artifact: &Path) -> Result<PathBuf> {
    let name = artifact_file_name(artifact)?;
    let parent = artifact_parent(artifact)?;
    Ok(parent.join(format!("{name}.partial")))
}

fn lock_path(artifact: &Path) -> Result<PathBuf> {
    let name = artifact_file_name(artifact)?;
    let parent = artifact_parent(artifact)?;
    Ok(parent.join(format!("{name}.lock")))
}

fn artifact_file_name(artifact: &Path) -> Result<String> {
    artifact
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .map(ToString::to_string)
        .ok_or_else(|| anyhow::anyhow!("refusing artifact path {}", artifact.display()))
}

fn artifact_parent(artifact: &Path) -> Result<&Path> {
    artifact
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| anyhow::anyhow!("artifact path {} has no parent", artifact.display()))
}

fn uqff_shard_names(dir: &Path) -> Vec<PathBuf> {
    let mut shards = Vec::new();
    let mut index = 0_u32;
    loop {
        let name = format!("{UQFF_STEM}-{index}.uqff");
        if !dir.join(&name).is_file() {
            break;
        }
        shards.push(PathBuf::from(name));
        index += 1;
    }
    shards
}

#[derive(Deserialize)]
struct ModuleManifestEntry {
    #[serde(default)]
    path: String,
    #[serde(rename = "type")]
    ty: String,
}

fn module_kind(ty: &str) -> &str {
    ty.rsplit('.').next().unwrap_or(ty)
}

fn module_extra_files(dir: &Path, modules_json: &[u8]) -> Result<Vec<PathBuf>> {
    let entries: Vec<ModuleManifestEntry> = serde_json::from_slice(modules_json)
        .context("parsing modules.json for UQFF bundle completeness")?;
    let mut files = Vec::new();
    for entry in entries {
        if entry.path.is_empty() {
            continue;
        }
        match module_kind(&entry.ty).to_ascii_lowercase().as_str() {
            "pooling" => files.push(dir.join(&entry.path).join("config.json")),
            "dense" => {
                files.push(dir.join(&entry.path).join("config.json"));
                files.push(dir.join(&entry.path).join("model.safetensors"));
            },
            _ => {},
        }
    }
    Ok(files)
}

fn uqff_bundle_missing(dir: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    if uqff_shard_names(dir).is_empty() {
        missing.push(format!("{UQFF_STEM}-0.uqff"));
    }
    for name in [
        RESIDUAL_FILE,
        "config.json",
        "tokenizer.json",
        "modules.json",
        IDENTITY_FILE,
    ] {
        if !dir.join(name).is_file() {
            missing.push(name.to_string());
        }
    }
    let modules_path = dir.join("modules.json");
    if let Ok(bytes) = std::fs::read(&modules_path) {
        match module_extra_files(dir, &bytes) {
            Ok(files) => {
                for file in files {
                    if !file.is_file() {
                        missing.push(file.display().to_string());
                    }
                }
            },
            Err(_) => missing.push("modules.json (invalid)".to_string()),
        }
    }
    missing
}

fn uqff_bundle_ready(dir: &Path) -> bool {
    uqff_bundle_missing(dir).is_empty()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn sha256_16(bytes: &[u8]) -> String {
    sha256_hex(bytes).chars().take(16).collect()
}

fn model_identity(spec: &str, config_bytes: &[u8]) -> String {
    format!("local-mistral:q8:{spec}:{}", sha256_16(config_bytes))
}

fn read_identity(dir: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(dir.join(IDENTITY_FILE))
        .with_context(|| format!("reading {}", dir.join(IDENTITY_FILE).display()))?;
    let identity = raw.trim();
    if identity.is_empty() {
        bail!("UQFF identity file is empty");
    }
    Ok(identity.to_string())
}

fn write_identity(dir: &Path, spec: &str) -> Result<String> {
    let config = std::fs::read(dir.join("config.json"))
        .with_context(|| format!("reading {} for identity", dir.join("config.json").display()))?;
    let identity = model_identity(spec, &config);
    std::fs::write(dir.join(IDENTITY_FILE), identity.as_bytes())
        .with_context(|| format!("writing {}", dir.join(IDENTITY_FILE).display()))?;
    Ok(identity)
}

fn resolve_hf_token_from(
    cli_token: Option<String>,
    env: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if let Some(token) = cli_token.filter(|token| !token.is_empty()) {
        return Some(token);
    }
    for key in ["HF_TOKEN", "HUGGINGFACE_API_KEY"] {
        if let Some(token) = env(key).filter(|token| !token.is_empty()) {
            return Some(token);
        }
    }
    None
}

fn resolve_hf_token(cli_token: Option<String>) -> Option<String> {
    resolve_hf_token_from(cli_token, |key| std::env::var(key).ok())
}

fn token_source(token: Option<String>) -> TokenSource {
    token.map_or(TokenSource::CacheToken, TokenSource::Literal)
}

fn open_artifact_lock(path: &Path) -> Result<fd_lock::RwLock<std::fs::File>> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    Ok(fd_lock::RwLock::new(file))
}

fn remove_dir_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

fn prepare_artifact_layout(planned: PathBuf) -> Result<(PathBuf, PathBuf, String)> {
    let name = artifact_file_name(&planned)?;
    let parent = artifact_parent(&planned)?.to_path_buf();
    std::fs::create_dir_all(&parent).with_context(|| format!("creating {}", parent.display()))?;
    let parent_real = parent
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", parent.display()))?;
    Ok((parent_real.join(&name), parent_real, name))
}

fn safe_remove_tree(path: &Path, trusted_parent: &Path, expected_name: &str) -> Result<()> {
    if path.file_name().and_then(|name| name.to_str()) != Some(expected_name) {
        bail!(
            "refusing to remove {} (expected name {expected_name})",
            path.display()
        );
    }
    let trusted = trusted_parent
        .canonicalize()
        .with_context(|| format!("canonicalizing trusted parent {}", trusted_parent.display()))?;
    if let Some(parent) = path.parent() {
        let parent_real = parent
            .canonicalize()
            .with_context(|| format!("canonicalizing parent {}", parent.display()))?;
        if parent_real != trusted {
            bail!(
                "refusing to remove {} (parent is not {})",
                path.display(),
                trusted.display()
            );
        }
    } else {
        bail!("refusing to remove {}", path.display());
    }
    match path.canonicalize() {
        Ok(real) => {
            if real.parent() != Some(trusted.as_path())
                || real.file_name().and_then(|name| name.to_str()) != Some(expected_name)
            {
                bail!(
                    "refusing to remove {} (resolves to {})",
                    path.display(),
                    real.display()
                );
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if path.symlink_metadata().is_err() {
                return Ok(());
            }
        },
        Err(error) => {
            return Err(error).with_context(|| format!("resolving {}", path.display()));
        },
    }
    remove_dir_if_exists(path)
}

fn publish_staging(
    staging: &Path,
    artifact: &Path,
    trusted_parent: &Path,
    name: &str,
) -> Result<()> {
    safe_remove_tree(artifact, trusted_parent, name)?;
    std::fs::rename(staging, artifact)
        .with_context(|| format!("publishing {} to {}", staging.display(), artifact.display()))
}

fn load_q8_model(mut args: ModelLoadArgs) -> Result<LoadedModel> {
    args.hf_token = resolve_hf_token(args.hf_token.take());
    let (artifact, parent_real, name) =
        prepare_artifact_layout(artifact_dir(&args.model_spec, args.cache_dir.as_deref()))?;
    let lock_file_path = lock_path(&artifact)?;
    let mut lock = open_artifact_lock(&lock_file_path)?;
    let _guard = lock
        .write()
        .map_err(|error| anyhow::anyhow!("locking UQFF artifact dir: {error}"))?;
    if uqff_bundle_ready(&artifact) {
        return load_uqff_bundle(&artifact);
    }
    if artifact.exists() || artifact.symlink_metadata().is_ok() {
        safe_remove_tree(&artifact, &parent_real, &name)?;
        std::fs::create_dir_all(&artifact)
            .with_context(|| format!("recreating {}", artifact.display()))?;
    }
    let staging = staging_dir(&artifact)?;
    let staging_name = artifact_file_name(&staging)?;
    safe_remove_tree(&staging, &parent_real, &staging_name)?;
    std::fs::create_dir_all(&staging).with_context(|| format!("creating {}", staging.display()))?;
    quantize_and_write_uqff(&args, &staging)?;
    write_identity(&staging, &args.model_spec)?;
    let missing = uqff_bundle_missing(&staging);
    if !missing.is_empty() {
        bail!(
            "UQFF bundle incomplete after quantization ({}): {}",
            staging.display(),
            missing.join(", ")
        );
    }
    publish_staging(&staging, &artifact, &parent_real, &name)?;
    load_uqff_bundle(&artifact)
}

fn load_uqff_bundle(artifact: &Path) -> Result<LoadedModel> {
    let shards = uqff_shard_names(artifact);
    if shards.is_empty() {
        bail!("UQFF bundle {} has no shards", artifact.display());
    }
    let identity = read_identity(artifact)?;
    #[cfg(feature = "tracing")]
    tracing::info!(path = %artifact.display(), shards = shards.len(), identity = %identity, "loading persisted Q8 UQFF embedding weights");
    let builder = UqffEmbeddingModelBuilder::new(artifact.display().to_string(), shards)
        .into_inner()
        .with_token_source(TokenSource::None)
        .with_force_cpu()
        .with_max_num_seqs(1)
        .with_device_mapping(cpu_device_map());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("creating UQFF load runtime")?;
    let model = runtime
        .block_on(builder.build())
        .map_err(|error| anyhow::anyhow!("failed to load Q8 UQFF embedding model: {error}"))?;
    Ok(LoadedModel { model, identity })
}

fn quantize_and_write_uqff(args: &ModelLoadArgs, staging: &Path) -> Result<()> {
    let output = staging.join(format!("{UQFF_STEM}.uqff"));
    let model_spec = args.model_spec.clone();
    let hf_token = args.hf_token.clone();
    let cache_dir = args.cache_dir.clone();
    #[cfg(feature = "tracing")]
    tracing::info!(path = %output.display(), "quantizing embedding model to Q8_0 and writing UQFF");
    let worker = std::thread::Builder::new()
        .name("chelix-embed-quantize".into())
        .spawn(move || {
            let mut builder = EmbeddingModelBuilder::new(&model_spec)
                .with_token_source(token_source(hf_token))
                .with_force_cpu()
                .with_max_num_seqs(1)
                .with_isq(IsqType::Q8_0)
                .write_uqff(output)
                .with_device_mapping(cpu_device_map());
            if let Some(cache_dir) = cache_dir {
                builder = builder.from_hf_cache_path(cache_dir);
            }
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("creating quantization runtime")?;
            runtime.block_on(builder.build()).map_err(|error| {
                anyhow::anyhow!("failed to quantize embedding model to Q8_0: {error}")
            })?;
            Ok(())
        })
        .context("starting quantization thread")?;
    match worker.join() {
        Ok(result) => result,
        Err(_) => bail!("quantization thread panicked"),
    }
}

fn refuse_gguf(model_spec: &str) -> Result<()> {
    let path = Path::new(model_spec);
    let looks_like_gguf = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
        || model_spec.to_ascii_lowercase().ends_with(".gguf");
    if looks_like_gguf {
        bail!(
            "embedding pipeline does not load GGUF; pass a Hugging Face snapshot directory or model id (mistral.rs GGUF is for text/multimodal generation)"
        );
    }
    Ok(())
}

async fn embed_prompt(model: &mistralrs::Model, prompt: &str) -> Result<Vec<f32>> {
    let embeddings = model
        .generate_embeddings(EmbeddingRequest::builder().add_prompt(prompt.to_owned()))
        .await
        .map_err(|error| anyhow::anyhow!("embedding failed: {error}"))?;
    take_single_embedding(embeddings)
}

async fn embed_tokens(
    model: &mistralrs::Model,
    tokens: &[u32],
    truncate_sequence: bool,
) -> Result<Vec<f32>> {
    let embeddings = model
        .generate_embeddings(
            EmbeddingRequest::builder()
                .add_tokens(tokens.to_vec())
                .with_truncate_sequence(truncate_sequence),
        )
        .await
        .map_err(|error| anyhow::anyhow!("embedding failed: {error}"))?;
    take_single_embedding(embeddings)
}

fn take_single_embedding(mut embeddings: Vec<Vec<f32>>) -> Result<Vec<f32>> {
    if embeddings.len() != 1 {
        bail!(
            "embedding model returned {} vectors; expected 1",
            embeddings.len()
        );
    }
    embeddings
        .pop()
        .context("embedding model returned an empty batch")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|error| panic!("mkdir: {error}"));
        }
        std::fs::write(path, contents).unwrap_or_else(|error| panic!("write: {error}"));
    }

    fn complete_bundle(dir: &Path, spec: &str) {
        write_file(&dir.join("chelix-q8_0-0.uqff"), "shard");
        write_file(&dir.join(RESIDUAL_FILE), "residual");
        write_file(&dir.join("config.json"), r#"{"model_type":"gemma"}"#);
        write_file(&dir.join("tokenizer.json"), "{}");
        write_file(
            &dir.join("modules.json"),
            r#"[{"idx":0,"name":"0","path":"","type":"sentence_transformers.models.Transformer"},{"idx":1,"name":"1","path":"1_Pooling","type":"sentence_transformers.models.Pooling"},{"idx":2,"name":"2","path":"2_Dense","type":"sentence_transformers.models.Dense"}]"#,
        );
        write_file(&dir.join("1_Pooling").join("config.json"), "{}");
        write_file(&dir.join("2_Dense").join("config.json"), "{}");
        write_file(&dir.join("2_Dense").join("model.safetensors"), "weights");
        write_identity(dir, spec).unwrap_or_else(|error| panic!("identity: {error}"));
    }

    #[test]
    fn identity_includes_engine_quant_spec_and_config_fingerprint() {
        let identity = model_identity("google/embeddinggemma-300m", b"{\"a\":1}");
        assert!(identity.starts_with("local-mistral:q8:google/embeddinggemma-300m:"));
        assert_eq!(
            identity.len(),
            "local-mistral:q8:google/embeddinggemma-300m:".len() + 16
        );
        assert_ne!(
            identity,
            model_identity("google/embeddinggemma-300m", b"{\"a\":2}")
        );
    }

    #[test]
    fn gguf_paths_are_rejected() {
        assert!(refuse_gguf("embeddinggemma-300M-Q8_0.gguf").is_err());
        assert!(refuse_gguf("/models/model.GGUF").is_err());
        assert!(refuse_gguf("google/embeddinggemma-300m").is_ok());
    }

    #[test]
    fn artifact_dir_for_snapshot_is_subdirectory() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
        let path = artifact_dir(
            dir.path().to_str().unwrap_or_else(|| panic!("utf8 path")),
            None,
        );
        assert_eq!(path, dir.path().join(UQFF_DIR_NAME));
    }

    #[test]
    fn artifact_dir_for_hf_id_uses_cache() {
        let cache = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
        let path = artifact_dir("google/embeddinggemma-300m", Some(cache.path()));
        assert_eq!(
            path,
            cache
                .path()
                .join("uqff")
                .join(artifact_key("google/embeddinggemma-300m"))
        );
        assert_eq!(artifact_key("google/embeddinggemma-300m").len(), 64);
    }

    #[test]
    fn artifact_dir_for_snapshot_with_cache_stays_in_cache() {
        let snapshot = tempfile::tempdir().unwrap_or_else(|error| panic!("snapshot: {error}"));
        let cache = tempfile::tempdir().unwrap_or_else(|error| panic!("cache: {error}"));
        let spec = snapshot
            .path()
            .to_str()
            .unwrap_or_else(|| panic!("utf8 path"));
        let path = artifact_dir(spec, Some(cache.path()));
        assert_eq!(path, cache.path().join("uqff").join(artifact_key(spec)));
        assert!(!path.starts_with(snapshot.path()));
    }

    #[test]
    fn artifact_key_does_not_depend_on_snapshot_dir_existing() {
        let root = tempfile::tempdir().unwrap_or_else(|error| panic!("root: {error}"));
        let real = root.path().join("real");
        std::fs::create_dir(&real).unwrap_or_else(|error| panic!("real dir: {error}"));
        let link = root.path().join("snapshot");
        std::os::unix::fs::symlink(&real, &link).unwrap_or_else(|error| panic!("symlink: {error}"));
        let spec = link.to_str().unwrap_or_else(|| panic!("utf8 path"));
        let canonical = real
            .canonicalize()
            .unwrap_or_else(|error| panic!("canonicalize: {error}"));

        let key_with_dir = artifact_key(spec);
        std::fs::remove_dir(&real).unwrap_or_else(|error| panic!("remove real: {error}"));
        let key_without_dir = artifact_key(spec);

        assert_eq!(key_with_dir, key_without_dir);
        assert_eq!(key_with_dir.len(), 64);
        assert_ne!(
            key_with_dir,
            artifact_key(canonical.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn dot_and_dotdot_specs_stay_inside_cache_uqff() {
        let cache = tempfile::tempdir().unwrap_or_else(|error| panic!("cache: {error}"));
        let canary = cache.path().join("canary.txt");
        write_file(&canary, "keep");
        let neighbor = cache.path().join("uqff").join("neighbor");
        write_file(&neighbor.join("keep.txt"), "keep");
        for spec in [".", ".."] {
            let path = artifact_dir(spec, Some(cache.path()));
            assert_eq!(
                path.parent().map(Path::as_os_str),
                Some(cache.path().join("uqff").as_os_str())
            );
            assert_eq!(path.file_name().map(|name| name.len()), Some(64));
            assert_ne!(path.file_name().and_then(|name| name.to_str()), Some(spec));
        }
        assert_eq!(std::fs::read_to_string(&canary).unwrap_or_default(), "keep");
        assert!(neighbor.join("keep.txt").is_file());
    }

    #[test]
    fn snapshot_without_cache_uses_chelix_uqff_name() {
        let snapshot = tempfile::tempdir().unwrap_or_else(|error| panic!("snapshot: {error}"));
        let spec = snapshot
            .path()
            .to_str()
            .unwrap_or_else(|| panic!("utf8 path"));
        let path = artifact_dir(spec, None);
        assert_eq!(path, snapshot.path().join(UQFF_DIR_NAME));
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(UQFF_DIR_NAME)
        );
    }

    #[test]
    fn safe_remove_refuses_symlink_outside_trusted_parent() {
        let parent = tempfile::tempdir().unwrap_or_else(|error| panic!("parent: {error}"));
        let outside = tempfile::tempdir().unwrap_or_else(|error| panic!("outside: {error}"));
        let victim = outside.path().join("victim");
        std::fs::create_dir(&victim).unwrap_or_else(|error| panic!("victim: {error}"));
        let link = parent.path().join(UQFF_DIR_NAME);
        std::os::unix::fs::symlink(&victim, &link)
            .unwrap_or_else(|error| panic!("symlink: {error}"));
        let error = match safe_remove_tree(&link, parent.path(), UQFF_DIR_NAME) {
            Err(error) => error,
            Ok(()) => panic!("symlink outside trusted parent must be refused"),
        };
        assert!(error.to_string().contains("refusing"));
        assert!(victim.is_dir());
    }

    #[test]
    fn safe_remove_clears_staging_sibling() {
        let parent = tempfile::tempdir().unwrap_or_else(|error| panic!("parent: {error}"));
        let staging = parent.path().join("chelix-uqff.partial");
        write_file(&staging.join("junk"), "x");
        safe_remove_tree(&staging, parent.path(), "chelix-uqff.partial")
            .unwrap_or_else(|error| panic!("remove staging: {error}"));
        assert!(!staging.exists());
    }

    #[test]
    fn uqff_shards_are_relative_filenames() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
        write_file(&dir.path().join("chelix-q8_0-0.uqff"), "shard");
        write_file(&dir.path().join("chelix-q8_0-1.uqff"), "shard");
        let shards = uqff_shard_names(dir.path());
        assert_eq!(shards, vec![
            PathBuf::from("chelix-q8_0-0.uqff"),
            PathBuf::from("chelix-q8_0-1.uqff")
        ]);
        assert!(shards.iter().all(|shard| shard.is_relative()));
        let relative_dir = PathBuf::from("rel-artifacts");
        assert_ne!(
            relative_dir.join(&shards[0]),
            PathBuf::from("rel-artifacts/rel-artifacts/chelix-q8_0-0.uqff")
        );
    }

    #[test]
    fn complete_bundle_is_ready_and_incomplete_is_not() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
        assert!(!uqff_bundle_ready(dir.path()));
        complete_bundle(dir.path(), "google/embeddinggemma-300m");
        assert!(uqff_bundle_ready(dir.path()));
        std::fs::remove_file(dir.path().join(RESIDUAL_FILE))
            .unwrap_or_else(|error| panic!("remove residual: {error}"));
        assert!(!uqff_bundle_ready(dir.path()));
    }

    #[test]
    fn incomplete_staging_dir_is_not_ready() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
        let staging = staging_dir(dir.path()).unwrap_or_else(|error| panic!("staging: {error}"));
        write_file(&staging.join("config.json"), "{}");
        write_file(&staging.join("chelix-q8_0-0.uqff"), "partial");
        assert!(!uqff_bundle_ready(&staging));
    }

    #[test]
    fn artifact_lock_can_be_reacquired_after_drop() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
        let path = lock_path(dir.path()).unwrap_or_else(|error| panic!("lock path: {error}"));
        let mut lock =
            open_artifact_lock(&path).unwrap_or_else(|error| panic!("open lock: {error}"));
        {
            let _guard = lock
                .write()
                .unwrap_or_else(|error| panic!("first lock: {error}"));
        }
        let _guard = lock
            .write()
            .unwrap_or_else(|error| panic!("second lock: {error}"));
    }

    #[test]
    fn token_priority_is_cli_then_hf_token_then_huggingface_api_key() {
        let env = |key: &str| match key {
            "HF_TOKEN" => Some("hf_env".to_string()),
            "HUGGINGFACE_API_KEY" => Some("hf_alias".to_string()),
            _ => None,
        };
        assert_eq!(
            resolve_hf_token_from(Some("cli".into()), env).as_deref(),
            Some("cli")
        );
        assert_eq!(
            resolve_hf_token_from(Some(String::new()), env).as_deref(),
            Some("hf_env")
        );
        let env_alias_only = |key: &str| match key {
            "HUGGINGFACE_API_KEY" => Some("hf_alias".to_string()),
            _ => None,
        };
        assert_eq!(
            resolve_hf_token_from(None, env_alias_only).as_deref(),
            Some("hf_alias")
        );
        assert_eq!(resolve_hf_token_from(None, |_| None), None);
    }
}
