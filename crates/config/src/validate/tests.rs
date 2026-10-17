use super::{
    ChelixConfig, Severity,
    schema_map::{KnownKeys, build_schema_map},
    validate_toml_str,
};

#[path = "tests/agents.rs"]
mod agents;
#[path = "tests/auxiliary.rs"]
mod auxiliary;
#[path = "tests/channels.rs"]
mod channels;
#[path = "tests/common.rs"]
mod common;
#[path = "tests/memory.rs"]
mod memory;
#[path = "tests/providers.rs"]
mod providers;
#[path = "tests/security.rs"]
mod security;
#[path = "tests/structural.rs"]
mod structural;
#[path = "tests/tools.rs"]
mod tools;
#[path = "tests/voice.rs"]
mod voice;
