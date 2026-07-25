use std::sync::Arc;

use crate::{
    AgentService, BrowserService, ChannelService, ChatService, CommandApprovalService,
    ConfigService, CronService, ExternalAgentService, LogsService, McpService, ModelService,
    NoopAgentService, NoopBrowserService, NoopChannelService, NoopChatService,
    NoopCommandApprovalService, NoopConfigService, NoopCronService, NoopExternalAgentService,
    NoopLogsService, NoopMcpService, NoopModelService, NoopOnboardingService, NoopProjectService,
    NoopProviderSetupService, NoopSessionService, NoopSkillsStub, NoopSttService,
    NoopSystemInfoService, NoopTtsService, NoopUpdateService, NoopUsageService,
    NoopVoicewakeService, NoopWebLoginService, OnboardingService, ProjectService,
    ProviderSetupService, SessionService, SkillsService, SttService, SystemInfoService, TtsService,
    UpdateService, UsageService, VoicewakeService, WebLoginService,
};

/// Bundle of all domain service trait objects.
///
/// Shared by the gateway (RPC), GraphQL, and any other transport layer.
/// Both sides call service methods directly through this struct.
pub struct Services {
    pub agent: Arc<dyn AgentService>,
    pub session: Arc<dyn SessionService>,
    pub channel: Arc<dyn ChannelService>,
    pub config: Arc<dyn ConfigService>,
    pub cron: Arc<dyn CronService>,
    pub chat: Arc<dyn ChatService>,
    pub tts: Arc<dyn TtsService>,
    pub stt: Arc<dyn SttService>,
    pub skills: Arc<dyn SkillsService>,
    pub mcp: Arc<dyn McpService>,
    pub browser: Arc<dyn BrowserService>,
    pub usage: Arc<dyn UsageService>,
    pub command_approval: Arc<dyn CommandApprovalService>,
    pub onboarding: Arc<dyn OnboardingService>,
    pub update: Arc<dyn UpdateService>,
    pub model: Arc<dyn ModelService>,
    pub web_login: Arc<dyn WebLoginService>,
    pub voicewake: Arc<dyn VoicewakeService>,
    pub logs: Arc<dyn LogsService>,
    pub provider_setup: Arc<dyn ProviderSetupService>,
    pub project: Arc<dyn ProjectService>,
    pub system_info: Arc<dyn SystemInfoService>,
    pub external_agent: Arc<dyn ExternalAgentService>,
}

impl Default for Services {
    fn default() -> Self {
        Self {
            agent: Arc::new(NoopAgentService),
            session: Arc::new(NoopSessionService),
            channel: Arc::new(NoopChannelService),
            config: Arc::new(NoopConfigService),
            cron: Arc::new(NoopCronService),
            chat: Arc::new(NoopChatService),
            tts: Arc::new(NoopTtsService),
            stt: Arc::new(NoopSttService),
            skills: Arc::new(NoopSkillsStub),
            mcp: Arc::new(NoopMcpService),
            browser: Arc::new(NoopBrowserService),
            usage: Arc::new(NoopUsageService),
            command_approval: Arc::new(NoopCommandApprovalService),
            onboarding: Arc::new(NoopOnboardingService),
            update: Arc::new(NoopUpdateService),
            model: Arc::new(NoopModelService),
            web_login: Arc::new(NoopWebLoginService),
            voicewake: Arc::new(NoopVoicewakeService),
            logs: Arc::new(NoopLogsService),
            provider_setup: Arc::new(NoopProviderSetupService),
            project: Arc::new(NoopProjectService),
            system_info: Arc::new(NoopSystemInfoService),
            external_agent: Arc::new(NoopExternalAgentService),
        }
    }
}
