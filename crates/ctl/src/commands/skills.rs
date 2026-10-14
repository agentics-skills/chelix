//! Skills management subcommands.

use {clap::Subcommand, serde_json::Value};

use crate::client::CtlClient;

#[derive(Subcommand)]
pub enum SkillsCommand {
    /// List all discovered skills.
    List,
    /// List bundled skill categories.
    Categories,
}

pub async fn run(client: &mut CtlClient, cmd: SkillsCommand) -> anyhow::Result<Value> {
    match cmd {
        SkillsCommand::List => client
            .call("skills.list", Value::Null)
            .await
            .map_err(Into::into),
        SkillsCommand::Categories => client
            .call("skills.bundled.categories", Value::Null)
            .await
            .map_err(Into::into),
    }
}
