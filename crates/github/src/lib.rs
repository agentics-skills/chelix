//! GitHub tool set for Chelix.
//!
//! Exposes the `github_*` agent tools backed by one shared authenticated
//! GitHub REST client. The personal access token comes from
//! `tools.github.pat`; tools that require it fail explicitly when it is
//! absent. Every GitHub HTTP request has the finite deadline configured by
//! `tools.github.request_timeout_secs`.

pub mod config_validation;
#[cfg(all(test, feature = "runtime"))]
mod test_support;

#[cfg(feature = "runtime")]
mod client;
#[cfg(feature = "runtime")]
mod error;
#[cfg(feature = "runtime")]
mod metrics;
#[cfg(feature = "runtime")]
mod rate_limit;
#[cfg(feature = "runtime")]
mod registration;
#[cfg(feature = "runtime")]
mod tools;

#[cfg(feature = "runtime")]
pub use {
    client::{GITHUB_API_BASE_URL, GitHubClient, GitHubResponse, RequestOptions},
    error::{Error, Result},
    registration::register_tools,
    tools::{
        GithubGetDirectoryContentsTool, GithubGetFileContentsTool, GithubGetLatestReleaseTool,
        GithubIssueReadTool, GithubListIssuesTool, GithubListPullRequestsTool,
        GithubListReleasesTool, GithubPullRequestReadTool, GithubSearchCodeTool,
        GithubSearchIssuesTool, GithubSearchRepositoriesTool,
    },
};
