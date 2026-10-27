//! Registration of the `github_*` tool set in an agent tool registry.

use std::sync::Arc;

use {chelix_agents::tool_registry::ToolRegistry, chelix_config_service::ConfigService};

use crate::{
    client::GitHubClient,
    tools::{
        GithubGetDirectoryContentsTool, GithubGetFileContentsTool, GithubGetLatestReleaseTool,
        GithubIssueReadTool, GithubListIssuesTool, GithubListPullRequestsTool,
        GithubListReleasesTool, GithubPullRequestReadTool, GithubSearchCodeTool,
        GithubSearchIssuesTool, GithubSearchRepositoriesTool,
    },
};

/// Register every `github_*` tool against one shared client.
///
/// The tools are always registered: a missing personal access token is
/// reported at call time as an explicit tool error rather than by silently
/// hiding the tools.
pub fn register_tools(registry: &mut ToolRegistry, config_service: ConfigService) {
    let client = Arc::new(GitHubClient::new(config_service));
    registry.register(Box::new(GithubGetDirectoryContentsTool::new(Arc::clone(
        &client,
    ))));
    registry.register(Box::new(GithubGetFileContentsTool::new(Arc::clone(
        &client,
    ))));
    registry.register(Box::new(GithubGetLatestReleaseTool::new(Arc::clone(
        &client,
    ))));
    registry.register(Box::new(GithubIssueReadTool::new(Arc::clone(&client))));
    registry.register(Box::new(GithubListIssuesTool::new(Arc::clone(&client))));
    registry.register(Box::new(GithubListPullRequestsTool::new(Arc::clone(
        &client,
    ))));
    registry.register(Box::new(GithubListReleasesTool::new(Arc::clone(&client))));
    registry.register(Box::new(GithubPullRequestReadTool::new(Arc::clone(
        &client,
    ))));
    registry.register(Box::new(GithubSearchCodeTool::new(Arc::clone(&client))));
    registry.register(Box::new(GithubSearchIssuesTool::new(Arc::clone(&client))));
    registry.register(Box::new(GithubSearchRepositoriesTool::new(client)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registers_the_complete_tool_set() {
        let mut registry = ToolRegistry::new();
        let fixture =
            chelix_agents::test_support::ConfigFixture::new(chelix_config::ChelixConfig::default())
                .await
                .unwrap_or_else(|error| panic!("config fixture failed: {error}"));
        register_tools(&mut registry, fixture.service.clone());

        let names: Vec<String> = registry
            .list_catalog()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, vec![
            "github_get_directory_contents".to_string(),
            "github_get_file_contents".to_string(),
            "github_get_latest_release".to_string(),
            "github_issue_read".to_string(),
            "github_list_issues".to_string(),
            "github_list_pull_requests".to_string(),
            "github_list_releases".to_string(),
            "github_pull_request_read".to_string(),
            "github_search_code".to_string(),
            "github_search_issues".to_string(),
            "github_search_repositories".to_string(),
        ]);
    }
}
