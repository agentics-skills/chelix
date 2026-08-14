//! Registration of the `github_*` tool set in an agent tool registry.

use std::sync::Arc;

use {chelix_agents::tool_registry::ToolRegistry, chelix_config::schema::GitHubConfig};

use crate::{
    client::GitHubClient,
    tools::{
        GithubGetDirectoryContentsTool, GithubGetFileContentsTool, GithubSearchCodeTool,
        GithubSearchRepositoriesTool,
    },
};

/// Register every `github_*` tool against one shared client.
///
/// The tools are always registered: a missing personal access token is
/// reported at call time as an explicit tool error rather than by silently
/// hiding the tools.
pub fn register_tools(registry: &mut ToolRegistry, config: &GitHubConfig) {
    let client = Arc::new(GitHubClient::new(
        config.pat.clone(),
        config.request_timeout_secs,
    ));
    registry.register(Box::new(GithubGetDirectoryContentsTool::new(Arc::clone(
        &client,
    ))));
    registry.register(Box::new(GithubGetFileContentsTool::new(Arc::clone(
        &client,
    ))));
    registry.register(Box::new(GithubSearchCodeTool::new(Arc::clone(&client))));
    registry.register(Box::new(GithubSearchRepositoriesTool::new(client)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_the_complete_tool_set() {
        let mut registry = ToolRegistry::new();
        register_tools(&mut registry, &GitHubConfig::default());

        let names: Vec<String> = registry
            .list_catalog()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, vec![
            "github_get_directory_contents".to_string(),
            "github_get_file_contents".to_string(),
            "github_search_code".to_string(),
            "github_search_repositories".to_string(),
        ]);
    }
}
