//! GitHub tool set for Chelix.
//!
//! Exposes the `github_*` agent tools backed by one shared authenticated
//! GitHub REST client. The personal access token comes from
//! `tools.github.pat`; tools that require it fail explicitly when it is
//! absent. Every GitHub HTTP request has the finite deadline configured by
//! `tools.github.request_timeout_secs`.

mod client;
mod error;
mod metrics;
mod rate_limit;
mod registration;
mod tools;

pub use {
    client::{GITHUB_API_BASE_URL, GitHubClient, GitHubResponse, RequestOptions},
    error::{Error, Result},
    registration::register_tools,
    tools::{
        GithubGetDirectoryContentsTool, GithubGetFileContentsTool, GithubSearchCodeTool,
        GithubSearchRepositoriesTool,
    },
};
