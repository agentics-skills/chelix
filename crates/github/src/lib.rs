//! GitHub tool set for Chelix.
//!
//! Exposes the `github_*` agent tools backed by one shared authenticated
//! GitHub REST client. The personal access token comes from
//! `tools.github.pat`; tools that require it fail explicitly when it is
//! absent.

mod client;
mod error;
mod metrics;
mod registration;
mod tools;

pub use {
    client::{GITHUB_API_BASE_URL, GitHubClient, GitHubResponse, RequestOptions},
    error::{Error, Result},
    registration::register_tools,
    tools::{GithubGetFileContentsTool, GithubSearchCodeTool},
};
