//! `github_*` agent tools.

mod get_directory_contents;
mod get_file_contents;
mod get_latest_release;
mod list_pull_requests;
mod list_releases;
mod pull_request_read;
mod request;
mod search_code;
mod search_repositories;

pub use {
    get_directory_contents::GithubGetDirectoryContentsTool,
    get_file_contents::GithubGetFileContentsTool, get_latest_release::GithubGetLatestReleaseTool,
    list_pull_requests::GithubListPullRequestsTool, list_releases::GithubListReleasesTool,
    pull_request_read::GithubPullRequestReadTool, search_code::GithubSearchCodeTool,
    search_repositories::GithubSearchRepositoriesTool,
};

use serde_json::Value;

use crate::error::{Error, Result};

/// Deserialize enriched tool parameters after dropping the runner's internal
/// underscore-prefixed context fields.
fn parse_params<T: serde::de::DeserializeOwned>(tool: &str, mut params: Value) -> Result<T> {
    let map = params
        .as_object_mut()
        .ok_or_else(|| Error::message(format!("{tool} parameters must be an object")))?;
    map.retain(|key, _| !key.starts_with('_'));
    serde_json::from_value(params)
        .map_err(|error| Error::message(format!("invalid {tool} parameters: {error}")))
}
