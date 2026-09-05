use {
    chelix_common::{ReasoningEffort, ResolvedModelReasoning},
    serde::{Deserialize, Serialize},
};

use crate::{Error, Result};

/// External agent transport kind persisted for a session binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalAgentKind {
    ClaudeCode,
    Opencode,
    Codex,
    PiAgent,
    Acp,
}

impl ExternalAgentKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Opencode => "opencode",
            Self::Codex => "codex",
            Self::PiAgent => "pi-agent",
            Self::Acp => "acp",
        }
    }
}

impl std::fmt::Display for ExternalAgentKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for ExternalAgentKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "claude-code" => Ok(Self::ClaudeCode),
            "opencode" => Ok(Self::Opencode),
            "codex" => Ok(Self::Codex),
            "pi-agent" => Ok(Self::PiAgent),
            "acp" => Ok(Self::Acp),
            other => Err(format!("unknown external agent kind: {other}")),
        }
    }
}

/// Canonical external identity attached to a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSessionIdentity {
    kind: ExternalAgentKind,
    external_session_id: Option<String>,
}

impl ExternalSessionIdentity {
    #[must_use]
    pub fn new(kind: ExternalAgentKind, external_session_id: Option<String>) -> Self {
        Self {
            kind,
            external_session_id,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ExternalAgentKind {
        self.kind
    }

    #[must_use]
    pub fn external_session_id(&self) -> Option<&str> {
        self.external_session_id.as_deref()
    }
}

/// Complete backing state for a persisted session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionBacking {
    Llm {
        model_reasoning: ResolvedModelReasoning,
    },
    LlmExternal {
        model_reasoning: ResolvedModelReasoning,
        external: ExternalSessionIdentity,
    },
    External {
        external: ExternalSessionIdentity,
    },
}

impl SessionBacking {
    #[must_use]
    pub fn llm(model_reasoning: ResolvedModelReasoning) -> Self {
        Self::Llm { model_reasoning }
    }

    #[must_use]
    pub fn external(external: ExternalSessionIdentity) -> Self {
        Self::External { external }
    }

    #[must_use]
    pub fn model_reasoning(&self) -> Option<&ResolvedModelReasoning> {
        match self {
            Self::Llm { model_reasoning }
            | Self::LlmExternal {
                model_reasoning, ..
            } => Some(model_reasoning),
            Self::External { .. } => None,
        }
    }

    #[must_use]
    pub fn model_id(&self) -> Option<&str> {
        self.model_reasoning().map(ResolvedModelReasoning::model_id)
    }

    #[must_use]
    pub fn reasoning_effort(&self) -> Option<&ReasoningEffort> {
        self.model_reasoning()
            .map(ResolvedModelReasoning::reasoning_effort)
    }

    #[must_use]
    pub const fn external_identity(&self) -> Option<&ExternalSessionIdentity> {
        match self {
            Self::Llm { .. } => None,
            Self::LlmExternal { external, .. } | Self::External { external } => Some(external),
        }
    }

    #[must_use]
    pub fn external_agent_kind(&self) -> Option<ExternalAgentKind> {
        self.external_identity().map(ExternalSessionIdentity::kind)
    }

    #[must_use]
    pub fn external_session_id(&self) -> Option<&str> {
        self.external_identity()
            .and_then(ExternalSessionIdentity::external_session_id)
    }

    pub(crate) fn try_from_persisted(
        model: Option<String>,
        reasoning_effort: Option<String>,
        external_agent_kind: Option<String>,
        external_session_id: Option<String>,
    ) -> Result<Self> {
        let external = match external_agent_kind {
            Some(kind) => {
                let kind = kind.parse().map_err(Error::message)?;
                Some(ExternalSessionIdentity::new(kind, external_session_id))
            },
            None if external_session_id.is_some() => {
                return Err(Error::message(
                    "persisted external session ID has no external agent kind",
                ));
            },
            None => None,
        };

        match (model, reasoning_effort, external) {
            (Some(model), Some(reasoning_effort), external) => {
                let model_reasoning =
                    ResolvedModelReasoning::try_new(model, ReasoningEffort::from(reasoning_effort))
                        .map_err(|error| Error::message(error.to_string()))?;
                Ok(match external {
                    Some(external) => Self::LlmExternal {
                        model_reasoning,
                        external,
                    },
                    None => Self::Llm { model_reasoning },
                })
            },
            (None, None, Some(external)) => Ok(Self::External { external }),
            (None, None, None) => Err(Error::message("persisted session has no backing")),
            _ => Err(Error::message(
                "persisted session has a partial model/reasoning pair",
            )),
        }
    }
}
