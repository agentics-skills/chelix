use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use {
    anyhow::{Context, Result, anyhow, bail},
    async_trait::async_trait,
    chelix_agents::tool_registry::{AgentTool, Truncation},
    serde::{Deserialize, Serialize},
    serde_json::{Map, Value},
    tokio::sync::oneshot,
    url::Url,
};

pub const PROTOCOL_VERSION: &str = "v0.9.1";
pub const BASIC_CATALOG_ID: &str =
    "https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json";
pub const TOOL_NAME: &str = "render_a2ui";

/// The component id the official surface element renders from.
const ROOT_COMPONENT_ID: &str = "root";

const MAX_REQUEST_BYTES: usize = 128 * 1024;
const MAX_ACTION_BYTES: usize = 32 * 1024;
const MAX_MESSAGES: usize = 64;
const MAX_COMPONENTS: usize = 200;
const MAX_JSON_DEPTH: usize = 32;
const MAX_JSON_NODES: usize = 5_000;
const MAX_STRING_BYTES: usize = 16 * 1024;
const MAX_ID_BYTES: usize = 128;
const MAX_ACTION_BUFFER: usize = 128;
const BUFFER_TTL: Duration = Duration::from_secs(30);

// The prompt's `Available Tools` catalog shows only the first 160 characters,
// so the opening sentence states what the tool does and what it returns. The
// protocol details follow for the full schema that `get_tool` reveals.
const TOOL_DESCRIPTION: &str = concat!(
    "Render an interactive UI in chat and wait for the user to act on it. Returns exactly ",
    "`{version, action}`. Uses A2UI v0.9.1 with the official basic catalog. The first message is ",
    "`createSurface` with the catalog ID, followed by `updateComponents` and optional ",
    "`updateDataModel` for the same surface. Exactly one component must have `\"id\": \"root\"` — ",
    "the renderer draws that component and everything it references, so a surface without it ",
    "stays blank. Component properties are flat: put fields such as `text`, `children`, `child`, ",
    "and `action` beside `id` and `component`; never use a nested `properties` object or inline ",
    "child components. Every component must carry the fields the catalog requires for it, and at ",
    "least one component must define an event `action` or the call is refused. `Image`, `Video`, ",
    "and `AudioPlayer` must reference a real media `url` served over `https:`, a `data:` URL, or ",
    "a root-relative chat path."
);

const MESSAGE_SCHEMA_DESCRIPTION: &str = concat!(
    "One A2UI message. Every message carries `version` plus EXACTLY ONE of `createSurface`, ",
    "`updateComponents`, or `updateDataModel` — never two of them in the same object. Split the ",
    "interaction across separate array items instead. `deleteSurface` is refused because this tool ",
    "waits for an action. Item 1 must be `createSurface`; every item must repeat the same ",
    "`surfaceId`. Minimal valid `messages`: ",
    r#"[{"version":"v0.9.1","createSurface":{"surfaceId":"confirm-order","catalogId":"https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json"}},{"version":"v0.9.1","updateComponents":{"surfaceId":"confirm-order","components":[{"id":"root","component":"Button","child":"label","variant":"primary","action":{"event":{"name":"confirm","context":{"approved":true}}}},{"id":"label","component":"Text","text":"Confirm"}]}}]"#
);

// Listed first in the component description, and phrased as a per-component
// contract, because models otherwise pick a plausible-looking property from the
// flat `properties` map (`children` on a `Card`, `label` on a `Button`) and the
// call is refused before it renders.
const REQUIRED_FIELDS_DESCRIPTION: &str = concat!(
    "EXACT required fields, by `component` value — a missing field means the call is refused: ",
    "`Text` -> `text`. ",
    "`Button` -> `child` (ONE component id, not `children`, not `label`) and `action`. ",
    "`Card` -> `child` (ONE component id, not `children`). ",
    "`Row`, `Column`, `List` -> `children` (array of component ids). ",
    "`Image`, `Video`, `AudioPlayer` -> `url`. ",
    "`Icon` -> `name`. ",
    "`Tabs` -> `tabs`. ",
    "`Modal` -> `trigger` and `content`. ",
    "`TextField` -> `label`. ",
    "`CheckBox` -> `label` and `value`. ",
    "`ChoicePicker` -> `options` and `value`. ",
    "`Slider` -> `max` and `value`. ",
    "`DateTimeInput` -> `value`. ",
    "`Divider` -> nothing. ",
    "`child` and `children` are mutually exclusive: a component uses whichever one is listed above ",
    "for its type. Visible button text is a separate `Text` component referenced by `child`."
);

const MEDIA_URL_DESCRIPTION: &str = concat!(
    "Required media source for `Image`, `Video`, and `AudioPlayer`. A literal URL must use ",
    "`https:`, a `data:` URL, or a root-relative chat path such as ",
    "`/api/sessions/<sessionKey>/media/<file>`. `http:` and other schemes are refused because the ",
    "chat content security policy blocks them. A standard data binding such as {\"path\":\"/photo\"} ",
    "is also accepted."
);

/// A component name from the trusted A2UI v0.9.1 basic catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum BasicComponent {
    AudioPlayer,
    Button,
    Card,
    CheckBox,
    ChoicePicker,
    Column,
    DateTimeInput,
    Divider,
    Icon,
    Image,
    List,
    Modal,
    Row,
    Slider,
    Tabs,
    Text,
    TextField,
    Video,
}

impl BasicComponent {
    const ALL: [Self; 18] = [
        Self::AudioPlayer,
        Self::Button,
        Self::Card,
        Self::CheckBox,
        Self::ChoicePicker,
        Self::Column,
        Self::DateTimeInput,
        Self::Divider,
        Self::Icon,
        Self::Image,
        Self::List,
        Self::Modal,
        Self::Row,
        Self::Slider,
        Self::Tabs,
        Self::Text,
        Self::TextField,
        Self::Video,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::AudioPlayer => "AudioPlayer",
            Self::Button => "Button",
            Self::Card => "Card",
            Self::CheckBox => "CheckBox",
            Self::ChoicePicker => "ChoicePicker",
            Self::Column => "Column",
            Self::DateTimeInput => "DateTimeInput",
            Self::Divider => "Divider",
            Self::Icon => "Icon",
            Self::Image => "Image",
            Self::List => "List",
            Self::Modal => "Modal",
            Self::Row => "Row",
            Self::Slider => "Slider",
            Self::Tabs => "Tabs",
            Self::Text => "Text",
            Self::TextField => "TextField",
            Self::Video => "Video",
        }
    }

    /// Fields the official basic-catalog schema marks as required for this
    /// component. A component missing one of them is silently dropped by the
    /// renderer, so the tool refuses the call instead.
    const fn required_fields(self) -> &'static [&'static str] {
        match self {
            Self::AudioPlayer | Self::Image | Self::Video => &["url"],
            Self::Button => &["child", "action"],
            Self::Card => &["child"],
            Self::CheckBox => &["label", "value"],
            Self::ChoicePicker => &["options", "value"],
            Self::Column | Self::List | Self::Row => &["children"],
            Self::DateTimeInput => &["value"],
            Self::Divider => &[],
            Self::Icon => &["name"],
            Self::Modal => &["trigger", "content"],
            Self::Slider => &["max", "value"],
            Self::Tabs => &["tabs"],
            Self::Text => &["text"],
            Self::TextField => &["label"],
        }
    }

    /// Whether this component renders a browser media element from `url`.
    const fn is_media(self) -> bool {
        matches!(self, Self::AudioPlayer | Self::Image | Self::Video)
    }
}

impl TryFrom<&str> for BasicComponent {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|component| component.name() == value)
            .with_context(|| {
                format!("component `{value}` is not in the trusted A2UI basic catalog")
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InteractionKey {
    pub session_key: String,
    pub run_id: String,
    pub tool_call_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct A2uiClientAction {
    pub name: String,
    #[serde(rename = "surfaceId")]
    pub surface_id: String,
    #[serde(rename = "sourceComponentId")]
    pub source_component_id: String,
    pub timestamp: String,
    pub context: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct A2uiClientMessage {
    pub version: String,
    pub action: A2uiClientAction,
}

impl A2uiClientMessage {
    pub fn parse(value: Value) -> Result<Self> {
        ensure_json_limits(&value, MAX_ACTION_BYTES)?;
        let message: Self =
            serde_json::from_value(value).context("invalid A2UI v0.9.1 client action message")?;
        if message.version != PROTOCOL_VERSION {
            bail!(
                "unsupported A2UI version `{}`; expected `{PROTOCOL_VERSION}`",
                message.version
            );
        }
        validate_identifier("action.name", &message.action.name)?;
        validate_identifier("action.surfaceId", &message.action.surface_id)?;
        validate_identifier(
            "action.sourceComponentId",
            &message.action.source_component_id,
        )?;
        time::OffsetDateTime::parse(
            &message.action.timestamp,
            &time::format_description::well_known::Rfc3339,
        )
        .context("action.timestamp must be RFC 3339")?;
        ensure_json_limits(
            &Value::Object(message.action.context.clone()),
            MAX_ACTION_BYTES,
        )?;
        Ok(message)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSurfaceMessage {
    version: String,
    #[serde(rename = "createSurface")]
    create_surface: CreateSurface,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSurface {
    #[serde(rename = "surfaceId")]
    surface_id: String,
    #[serde(rename = "catalogId")]
    catalog_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theme: Option<Value>,
    #[serde(
        rename = "sendDataModel",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    send_data_model: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateComponentsMessage {
    version: String,
    #[serde(rename = "updateComponents")]
    update_components: UpdateComponents,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateComponents {
    #[serde(rename = "surfaceId")]
    surface_id: String,
    components: Vec<A2uiComponent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct A2uiComponent {
    component: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    weight: Option<f64>,
    #[serde(flatten)]
    properties: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateDataModelMessage {
    version: String,
    #[serde(rename = "updateDataModel")]
    update_data_model: UpdateDataModel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateDataModel {
    #[serde(rename = "surfaceId")]
    surface_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteSurfaceMessage {
    version: String,
    #[serde(rename = "deleteSurface")]
    delete_surface: DeleteSurface,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteSurface {
    #[serde(rename = "surfaceId")]
    surface_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum A2uiServerMessage {
    Create(CreateSurfaceMessage),
    UpdateComponents(UpdateComponentsMessage),
    UpdateDataModel(UpdateDataModelMessage),
    Delete(DeleteSurfaceMessage),
}

impl A2uiServerMessage {
    fn version(&self) -> &str {
        match self {
            Self::Create(message) => &message.version,
            Self::UpdateComponents(message) => &message.version,
            Self::UpdateDataModel(message) => &message.version,
            Self::Delete(message) => &message.version,
        }
    }

    fn surface_id(&self) -> &str {
        match self {
            Self::Create(message) => &message.create_surface.surface_id,
            Self::UpdateComponents(message) => &message.update_components.surface_id,
            Self::UpdateDataModel(message) => &message.update_data_model.surface_id,
            Self::Delete(message) => &message.delete_surface.surface_id,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum A2uiServerMessageKind {
    Create,
    UpdateComponents,
    UpdateDataModel,
    Delete,
}

impl A2uiServerMessageKind {
    const ALL: [Self; 4] = [
        Self::Create,
        Self::UpdateComponents,
        Self::UpdateDataModel,
        Self::Delete,
    ];

    const fn field(self) -> &'static str {
        match self {
            Self::Create => "createSurface",
            Self::UpdateComponents => "updateComponents",
            Self::UpdateDataModel => "updateDataModel",
            Self::Delete => "deleteSurface",
        }
    }

    fn parse(self, value: &Value, position: usize) -> Result<A2uiServerMessage> {
        let invalid = |error: serde_json::Error| {
            anyhow!(
                "A2UI message {position} `{}` is invalid: {error}",
                self.field()
            )
        };
        match self {
            Self::Create => serde_json::from_value(value.clone())
                .map(A2uiServerMessage::Create)
                .map_err(invalid),
            Self::UpdateComponents => serde_json::from_value(value.clone())
                .map(A2uiServerMessage::UpdateComponents)
                .map_err(invalid),
            Self::UpdateDataModel => serde_json::from_value(value.clone())
                .map(A2uiServerMessage::UpdateDataModel)
                .map_err(invalid),
            Self::Delete => serde_json::from_value(value.clone())
                .map(A2uiServerMessage::Delete)
                .map_err(invalid),
        }
    }
}

struct ValidatedInteraction {
    surface_id: String,
}

fn parse_server_message(value: &Value, index: usize) -> Result<A2uiServerMessage> {
    let position = index + 1;
    let object = value
        .as_object()
        .with_context(|| format!("A2UI message {position} must be an object"))?;
    let kinds = A2uiServerMessageKind::ALL
        .into_iter()
        .filter(|kind| object.contains_key(kind.field()))
        .collect::<Vec<_>>();
    let [kind] = kinds.as_slice() else {
        let supplied = object
            .keys()
            .filter(|key| key.as_str() != "version")
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("`, `");
        let supplied = if supplied.is_empty() {
            "none".to_string()
        } else {
            format!("`{supplied}`")
        };
        bail!(
            "A2UI message {position} must contain exactly one standard message field \
             (`createSurface`, `updateComponents`, `updateDataModel`, or `deleteSurface`); \
             found {supplied}"
        );
    };
    kind.parse(value, position)
}

fn validate_render_params(params: &Value) -> Result<ValidatedInteraction> {
    ensure_json_limits(params, MAX_REQUEST_BYTES)?;
    let object = params
        .as_object()
        .context("render_a2ui parameters must be an object")?;
    for key in object.keys() {
        if matches!(key.as_str(), "messages" | "active_tools" | "tool_choice")
            || key.starts_with('_')
        {
            continue;
        }
        bail!("unknown render_a2ui parameter `{key}`");
    }

    let raw_messages = object
        .get("messages")
        .context("missing required render_a2ui parameter `messages`")?
        .as_array()
        .context("render_a2ui parameter `messages` must be an array")?;
    if raw_messages.is_empty() || raw_messages.len() > MAX_MESSAGES {
        bail!("messages must contain between 1 and {MAX_MESSAGES} A2UI messages");
    }
    let messages = raw_messages
        .iter()
        .enumerate()
        .map(|(index, message)| parse_server_message(message, index))
        .collect::<Result<Vec<_>>>()?;

    let Some(A2uiServerMessage::Create(create)) = messages.first() else {
        bail!("the first A2UI message must be createSurface");
    };
    if create.create_surface.catalog_id != BASIC_CATALOG_ID {
        bail!(
            "unsupported A2UI catalog `{}`; expected `{BASIC_CATALOG_ID}`",
            create.create_surface.catalog_id
        );
    }
    let surface_id = create.create_surface.surface_id.clone();
    validate_identifier("surfaceId", &surface_id)?;

    let mut create_count = 0usize;
    let mut component_count = 0usize;
    let mut has_server_action = false;
    let mut declared_components: Map<String, Value> = Map::new();
    let mut modal_triggers: Vec<(String, Value)> = Vec::new();
    for message in &messages {
        if message.version() != PROTOCOL_VERSION {
            bail!(
                "unsupported A2UI version `{}`; expected `{PROTOCOL_VERSION}`",
                message.version()
            );
        }
        if message.surface_id() != surface_id {
            bail!("all A2UI messages in one interaction must target `{surface_id}`");
        }
        match message {
            A2uiServerMessage::Create(_) => create_count += 1,
            A2uiServerMessage::UpdateComponents(update) => {
                if update.update_components.components.is_empty() {
                    bail!("updateComponents.components must not be empty");
                }
                component_count = component_count
                    .checked_add(update.update_components.components.len())
                    .context("component count overflow")?;
                if component_count > MAX_COMPONENTS {
                    bail!("A2UI interaction exceeds the {MAX_COMPONENTS} component limit");
                }
                for component in &update.update_components.components {
                    validate_component(component)?;
                    has_server_action |= contains_server_action(&component.properties);
                    if let Some(id) = component.id.as_deref() {
                        declared_components
                            .insert(id.to_string(), Value::Object(component.properties.clone()));
                    }
                    if BasicComponent::try_from(component.component.as_str())?
                        == BasicComponent::Modal
                        && let Some(trigger) = component.properties.get("trigger")
                    {
                        modal_triggers.push((
                            component
                                .id
                                .clone()
                                .unwrap_or_else(|| component.component.clone()),
                            trigger.clone(),
                        ));
                    }
                }
            },
            A2uiServerMessage::UpdateDataModel(update) => {
                if let Some(path) = update.update_data_model.path.as_deref()
                    && path.len() > MAX_STRING_BYTES
                {
                    bail!("updateDataModel.path exceeds {MAX_STRING_BYTES} bytes");
                }
            },
            A2uiServerMessage::Delete(_) => {
                bail!("deleteSurface is not valid in an interaction that is awaiting an action");
            },
        }
    }
    if create_count != 1 {
        bail!("an interaction must contain exactly one createSurface message");
    }
    if component_count == 0 {
        bail!("an interaction must define at least one component");
    }
    // The official surface element renders `renderA2uiNode` from the component
    // whose id is exactly `root` and otherwise shows "Loading surface..."
    // forever. Without this check the tool would sit there until its timeout
    // with nothing on screen.
    if !declared_components.contains_key(ROOT_COMPONENT_ID) {
        bail!(
            "an interaction must declare the entry component with `\"id\": \"{ROOT_COMPONENT_ID}\"`; \
             the renderer starts from that component and shows nothing without it"
        );
    }
    if !has_server_action {
        bail!("an interaction awaiting user input must contain an A2UI event action");
    }
    for (modal_label, trigger) in &modal_triggers {
        validate_modal_trigger(modal_label, trigger, &declared_components)?;
    }

    Ok(ValidatedInteraction { surface_id })
}

pub fn surface_id_from_tool_arguments(params: &Value) -> Result<String> {
    validate_render_params(params).map(|interaction| interaction.surface_id)
}

/// Validate one flat basic-catalog component against the trusted catalog, the
/// fields the official renderer requires, and the chat media policy.
fn validate_component(component: &A2uiComponent) -> Result<()> {
    let kind = BasicComponent::try_from(component.component.as_str())?;
    let label = component
        .id
        .as_deref()
        .unwrap_or(component.component.as_str());
    if let Some(id) = component.id.as_deref() {
        validate_identifier("component.id", id)?;
    }
    if component.properties.contains_key("properties") {
        bail!(
            "component `{label}` uses non-standard nested `properties`; put component-specific \
             fields directly beside `id` and `component`"
        );
    }
    for field in kind.required_fields() {
        if !component.properties.contains_key(*field) {
            bail!(
                "component `{label}` of type `{}` is missing the required field `{field}`",
                kind.name()
            );
        }
    }
    if kind.is_media() {
        let url = component
            .properties
            .get("url")
            .with_context(|| format!("component `{label}` is missing the required field `url`"))?;
        validate_media_url(label, kind, url)?;
    }
    Ok(())
}

/// Accept only media sources the chat page can actually load: a `data:` URL, a
/// root-relative chat path, an `https:` URL, or a standard data binding.
fn validate_media_url(label: &str, kind: BasicComponent, url: &Value) -> Result<()> {
    let Some(literal) = url.as_str() else {
        if url.is_object() {
            return Ok(());
        }
        bail!(
            "component `{label}` of type `{}` must set `url` to a URL string or a standard data \
             binding",
            kind.name()
        );
    };
    let literal = literal.trim();
    if literal.is_empty() {
        bail!(
            "component `{label}` of type `{}` has an empty `url`",
            kind.name()
        );
    }
    if literal.starts_with("data:") {
        return Ok(());
    }
    if literal.starts_with('/') && !literal.starts_with("//") {
        return Ok(());
    }
    let parsed = Url::parse(literal).with_context(|| {
        format!(
            "component `{label}` of type `{}` has an invalid `url`",
            kind.name()
        )
    })?;
    if parsed.scheme() != "https" {
        bail!(
            "component `{label}` of type `{}` uses the `{}:` scheme; the chat content security \
             policy only loads `https:`, `data:`, and root-relative media",
            kind.name(),
            parsed.scheme()
        );
    }
    Ok(())
}

fn contains_server_action(properties: &Map<String, Value>) -> bool {
    properties
        .get("action")
        .and_then(Value::as_object)
        .and_then(|action| action.get("event"))
        .and_then(Value::as_object)
        .and_then(|event| event.get("name"))
        .and_then(Value::as_str)
        .is_some_and(|name| !name.trim().is_empty())
}

/// A `Modal` trigger only opens the dialog; the official renderer wraps it in
/// its own click handler. If the trigger component also carries a server
/// action, the single click both opens the modal and completes the
/// interaction, which locks the surface while the dialog stays open and makes
/// the modal content unreachable. Refuse that payload instead of shipping a
/// dead-end dialog.
fn validate_modal_trigger(
    modal_label: &str,
    trigger: &Value,
    declared_components: &Map<String, Value>,
) -> Result<()> {
    let trigger_id = trigger.as_str().with_context(|| {
        format!(
            "component `{modal_label}` of type `Modal` must set `trigger` to the id of a declared \
             component"
        )
    })?;
    let Some(properties) = declared_components
        .get(trigger_id)
        .and_then(Value::as_object)
    else {
        bail!(
            "component `{modal_label}` of type `Modal` references the undeclared trigger \
             component `{trigger_id}`"
        );
    };
    if contains_server_action(properties) {
        bail!(
            "component `{modal_label}` of type `Modal` uses trigger `{trigger_id}` that also \
             sends an action; opening the modal would end the interaction before its content is \
             reachable. Use a non-interactive trigger such as `Text`, `Icon`, or `Card` and put \
             the action on a `Button` inside `content`"
        );
    }
    Ok(())
}

fn validate_identifier(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{label} must not be empty");
    }
    if value.len() > MAX_ID_BYTES {
        bail!("{label} exceeds {MAX_ID_BYTES} bytes");
    }
    if value.chars().any(char::is_control) {
        bail!("{label} must not contain control characters");
    }
    Ok(())
}

fn ensure_json_limits(value: &Value, max_bytes: usize) -> Result<()> {
    let bytes = serde_json::to_vec(value).context("failed to measure JSON payload")?;
    if bytes.len() > max_bytes {
        bail!("JSON payload exceeds {max_bytes} bytes");
    }
    let mut nodes = 0usize;
    inspect_json(value, 0, &mut nodes)
}

fn inspect_json(value: &Value, depth: usize, nodes: &mut usize) -> Result<()> {
    if depth > MAX_JSON_DEPTH {
        bail!("JSON payload exceeds maximum depth {MAX_JSON_DEPTH}");
    }
    *nodes = nodes.checked_add(1).context("JSON node count overflow")?;
    if *nodes > MAX_JSON_NODES {
        bail!("JSON payload exceeds {MAX_JSON_NODES} nodes");
    }
    match value {
        Value::String(text) if text.len() > MAX_STRING_BYTES => {
            bail!("JSON string exceeds {MAX_STRING_BYTES} bytes")
        },
        Value::Array(values) => {
            for child in values {
                inspect_json(child, depth + 1, nodes)?;
            }
        },
        Value::Object(object) => {
            for (key, child) in object {
                if key.len() > MAX_STRING_BYTES {
                    bail!("JSON object key exceeds {MAX_STRING_BYTES} bytes");
                }
                inspect_json(child, depth + 1, nodes)?;
            }
        },
        _ => {},
    }
    Ok(())
}

struct BufferedAction {
    message: A2uiClientMessage,
    created_at: Instant,
}

struct PendingWaiter {
    id: u64,
    sender: oneshot::Sender<A2uiClientMessage>,
}

enum BrokerEntry {
    Buffered(BufferedAction),
    Waiting(PendingWaiter),
    Completed(Instant),
}

#[derive(Default)]
struct BrokerState {
    entries: HashMap<InteractionKey, BrokerEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum BrokerSubmitError {
    #[error("an action was already submitted for this A2UI interaction")]
    Duplicate,
    #[error("the A2UI interaction is no longer waiting for an action")]
    Closed,
    #[error("the A2UI early-action buffer is full")]
    BufferFull,
}

#[derive(Default)]
pub struct A2uiBroker {
    state: Mutex<BrokerState>,
    next_waiter_id: AtomicU64,
}

impl A2uiBroker {
    pub fn submit(
        &self,
        key: InteractionKey,
        message: A2uiClientMessage,
    ) -> std::result::Result<(), BrokerSubmitError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        prune_buffered_actions(&mut state);
        match state.entries.remove(&key) {
            Some(BrokerEntry::Waiting(waiter)) => {
                state
                    .entries
                    .insert(key, BrokerEntry::Completed(Instant::now()));
                waiter
                    .sender
                    .send(message)
                    .map_err(|_| BrokerSubmitError::Closed)
            },
            Some(BrokerEntry::Buffered(buffered)) => {
                state.entries.insert(key, BrokerEntry::Buffered(buffered));
                Err(BrokerSubmitError::Duplicate)
            },
            Some(BrokerEntry::Completed(completed_at)) => {
                state
                    .entries
                    .insert(key, BrokerEntry::Completed(completed_at));
                Err(BrokerSubmitError::Duplicate)
            },
            None if buffered_count(&state) >= MAX_ACTION_BUFFER => {
                Err(BrokerSubmitError::BufferFull)
            },
            None => {
                state.entries.insert(
                    key,
                    BrokerEntry::Buffered(BufferedAction {
                        message,
                        created_at: Instant::now(),
                    }),
                );
                Ok(())
            },
        }
    }

    /// Wait for the user to act on a surface.
    ///
    /// There is no deadline: the operator may be away from the chat, and a
    /// surface that expires on its own leaves the agent with an outcome the
    /// user never chose. The wait ends when an action arrives, or when the run
    /// is stopped and the future is dropped.
    async fn wait(self: &Arc<Self>, key: InteractionKey) -> Result<A2uiClientMessage> {
        let waiter_id = self.next_waiter_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            prune_buffered_actions(&mut state);
            match state.entries.remove(&key) {
                Some(BrokerEntry::Buffered(buffered)) => {
                    state
                        .entries
                        .insert(key, BrokerEntry::Completed(Instant::now()));
                    return Ok(buffered.message);
                },
                Some(BrokerEntry::Waiting(waiter)) => {
                    state.entries.insert(key, BrokerEntry::Waiting(waiter));
                    bail!("another render_a2ui call is already waiting with this interaction key");
                },
                Some(BrokerEntry::Completed(completed_at)) => {
                    state
                        .entries
                        .insert(key, BrokerEntry::Completed(completed_at));
                    bail!("this A2UI interaction has already completed");
                },
                None => {
                    state.entries.insert(
                        key.clone(),
                        BrokerEntry::Waiting(PendingWaiter {
                            id: waiter_id,
                            sender,
                        }),
                    );
                },
            }
        }

        let mut cleanup = WaiterCleanup {
            broker: Arc::clone(self),
            key,
            waiter_id,
            armed: true,
        };
        let result = receiver.await;
        cleanup.remove();
        match result {
            Ok(message) => Ok(message),
            Err(_) => bail!("A2UI action wait was cancelled"),
        }
    }
}

fn prune_buffered_actions(state: &mut BrokerState) {
    state.entries.retain(|_, entry| match entry {
        BrokerEntry::Buffered(buffered) => buffered.created_at.elapsed() < BUFFER_TTL,
        BrokerEntry::Waiting(_) => true,
        BrokerEntry::Completed(completed_at) => completed_at.elapsed() < BUFFER_TTL,
    });
}

fn buffered_count(state: &BrokerState) -> usize {
    state
        .entries
        .values()
        .filter(|entry| matches!(entry, BrokerEntry::Buffered(_)))
        .count()
}

struct WaiterCleanup {
    broker: Arc<A2uiBroker>,
    key: InteractionKey,
    waiter_id: u64,
    armed: bool,
}

impl WaiterCleanup {
    fn remove(&mut self) {
        if !self.armed {
            return;
        }
        let mut state = self
            .broker
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if matches!(
            state.entries.get(&self.key),
            Some(BrokerEntry::Waiting(waiter)) if waiter.id == self.waiter_id
        ) {
            state
                .entries
                .insert(self.key.clone(), BrokerEntry::Completed(Instant::now()));
        }
        self.armed = false;
    }
}

impl Drop for WaiterCleanup {
    fn drop(&mut self) {
        self.remove();
    }
}

pub struct RenderA2uiTool {
    broker: Arc<A2uiBroker>,
}

impl RenderA2uiTool {
    pub fn new(broker: Arc<A2uiBroker>) -> Self {
        Self { broker }
    }
}

fn identifier_schema() -> Value {
    serde_json::json!({
        "type": "string",
        "minLength": 1,
        "maxLength": MAX_ID_BYTES
    })
}

fn event_action_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "description": "Standard server event action required on at least one interactive component.",
        "properties": {
            "event": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": identifier_schema(),
                    "context": {
                        "type": "object",
                        "description": "Optional action values; values may be literals, arrays, or standard data bindings such as {\"path\":\"/field\"}."
                    }
                },
                "required": ["name"]
            }
        },
        "required": ["event"]
    })
}

fn component_schema() -> Value {
    let component_names = BasicComponent::ALL
        .into_iter()
        .map(BasicComponent::name)
        .collect::<Vec<_>>();
    serde_json::json!({
        "type": "object",
        "description": format!(
            "{REQUIRED_FIELDS_DESCRIPTION} \
             The object is flat: component-specific fields sit beside `id` and `component`, and \
             children are referenced by id only — never inline a child object. Input values may \
             use data bindings such as {{\"path\":\"/field\"}} once updateDataModel initializes \
             that path."
        ),
        "properties": {
            "id": identifier_schema(),
            "component": {
                "type": "string",
                "enum": component_names
            },
            "weight": { "type": "number" },
            "child": {
                "type": "string",
                "description": "ID of ONE separately declared component. Required by `Button` and `Card`; those two never take `children`."
            },
            "children": {
                "description": "Array of separately declared component IDs, or the standard ChildList template object. Required by `Row`, `Column`, and `List`; those three never take `child`."
            },
            "text": {
                "description": "Text literal or standard DynamicString binding for Text."
            },
            "label": {
                "description": "Label literal or standard DynamicString binding for `TextField`, `CheckBox`, or `ChoicePicker`. Not a button caption: a `Button` shows its text through `child`."
            },
            "value": {
                "description": "Literal value or standard data binding accepted by the selected component."
            },
            "variant": {
                "type": "string",
                "description": "A variant supported by the selected basic-catalog component."
            },
            "url": {
                "description": MEDIA_URL_DESCRIPTION
            },
            "description": {
                "description": "Accessibility description for Image, or a title for AudioPlayer."
            },
            "fit": {
                "type": "string",
                "enum": ["contain", "cover", "fill", "none", "scaleDown"],
                "description": "How an Image is resized inside its box."
            },
            "name": {
                "description": "The catalog icon name for Icon, or the standard {\"svgPath\":\"…\"} object."
            },
            "options": {
                "type": "array",
                "description": "ChoicePicker options; each entry is {\"label\":…,\"value\":…}. \
                                The renderer always writes the selection back as an array of \
                                values, including for `mutuallyExclusive`, so bind `value` to a \
                                data-model path and initialize that path with an array. A scalar \
                                initial value is returned unchanged when the user never touches \
                                the control, which makes the response shape inconsistent.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "label": { "description": "The text shown for this option." },
                        "value": {
                            "type": "string",
                            "description": "The stable value reported when this option is selected."
                        }
                    },
                    "required": ["label", "value"]
                }
            },
            "tabs": {
                "type": "array",
                "description": "Tabs entries; each entry is {\"title\":…,\"child\":\"<component id>\"}.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "title": { "description": "The tab title." },
                        "child": {
                            "type": "string",
                            "description": "ID of the component rendered inside this tab."
                        }
                    },
                    "required": ["title", "child"]
                }
            },
            "trigger": {
                "type": "string",
                "description": "ID of the component that opens a Modal. It must not carry an \
                                `action`: the renderer opens the dialog on click, so a trigger \
                                that also sends an event ends the interaction before the dialog \
                                content can be used. Use `Text`, `Icon`, or `Card` as the \
                                trigger and put the `Button` with the `action` inside `content`."
            },
            "content": {
                "type": "string",
                "description": "ID of the component rendered inside a Modal. Put the acting \
                                `Button` here."
            },
            "min": {
                "description": "Minimum value for Slider or DateTimeInput."
            },
            "max": {
                "description": "Maximum value; required by Slider."
            },
            "step": {
                "type": "number",
                "description": "Slider step size. The renderer reports the exact dragged value \
                                and does not snap it to this step, so treat the returned number \
                                as continuous between `min` and `max`."
            },
            "action": event_action_schema()
        },
        "required": ["component"]
    })
}

fn create_surface_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "surfaceId": identifier_schema(),
            "catalogId": {
                "type": "string",
                "enum": [BASIC_CATALOG_ID]
            },
            "theme": {
                "description": "Optional standard A2UI theme parameters."
            },
            "sendDataModel": { "type": "boolean" }
        },
        "required": ["surfaceId", "catalogId"]
    })
}

fn update_components_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "surfaceId": identifier_schema(),
            "components": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_COMPONENTS,
                "description": "Flat basic-catalog component objects. One of them must have `\"id\": \"root\"`: the renderer starts there and draws only what that component references. Never use a nested `properties` object or inline components. Row, Column, and List use `children` containing component IDs; Card and Button use one `child` ID. Text uses `text`. A responding Button uses `action.event.name` and optional `action.event.context`.",
                "items": component_schema()
            }
        },
        "required": ["surfaceId", "components"]
    })
}

fn update_data_model_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "surfaceId": identifier_schema(),
            "path": {
                "type": "string",
                "description": "Optional JSON Pointer path within the surface data model."
            },
            "value": {
                "description": "The standard A2UI data-model value at path, or the root value when path is omitted."
            }
        },
        "required": ["surfaceId"]
    })
}

fn delete_surface_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "description": "Standard A2UI delete message; rejected by render_a2ui because the tool must keep the surface while waiting for an action.",
        "properties": {
            "surfaceId": identifier_schema()
        },
        "required": ["surfaceId"]
    })
}

// Deliberately flat instead of a `oneOf` over the four message kinds. Models
// answer union branches with merged objects far more often than they do a flat
// shape, and `deleteSurface` is refused outright here, so a union would mostly
// advertise a branch that is never valid. The one-field-per-message rule is
// stated in the description and enforced by `parse_server_message`, which
// names the offending message and field.
fn server_message_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "description": MESSAGE_SCHEMA_DESCRIPTION,
        "properties": {
            "version": {
                "type": "string",
                "enum": [PROTOCOL_VERSION],
                "description": "The exact A2UI protocol version. Include the leading `v`."
            },
            "createSurface": create_surface_schema(),
            "updateComponents": update_components_schema(),
            "updateDataModel": update_data_model_schema(),
            "deleteSurface": delete_surface_schema()
        },
        "required": ["version"]
    })
}

#[async_trait]
impl AgentTool for RenderA2uiTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "messages": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_MESSAGES,
                    "description": "A2UI v0.9.1 server-to-client messages for one surface using the official basic catalog. Do not send deleteSurface because this tool waits for an action.",
                    "items": server_message_schema()
                }
            },
            "required": ["messages"]
        })
    }

    fn validate(&self, params: &Value) -> Result<()> {
        validate_render_params(params).map(|_| ())
    }

    fn truncation(&self, _params: &Value) -> Truncation {
        Truncation::Off
    }

    async fn execute(&self, params: Value) -> Result<Value> {
        let interaction = validate_render_params(&params)?;
        let object = params
            .as_object()
            .context("render_a2ui parameters must be an object")?;
        let required_context = |name: &str| -> Result<String> {
            object
                .get(name)
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToString::to_string)
                .with_context(|| format!("missing trusted `{name}` execution context"))
        };
        let key = InteractionKey {
            session_key: required_context("_session_key")?,
            run_id: required_context("_run_id")?,
            tool_call_id: required_context("_tool_call_id")?,
        };
        let message = self.broker.wait(key).await?;
        if message.action.surface_id != interaction.surface_id {
            bail!(
                "received A2UI action for surface `{}` while waiting for `{}`",
                message.action.surface_id,
                interaction.surface_id
            );
        }
        serde_json::to_value(message).context("failed to serialize A2UI action")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages() -> Value {
        serde_json::json!([
            {
                "version": "v0.9.1",
                "createSurface": {
                    "surfaceId": "confirm-order",
                    "catalogId": BASIC_CATALOG_ID
                }
            },
            {
                "version": "v0.9.1",
                "updateComponents": {
                    "surfaceId": "confirm-order",
                    "components": [
                        {
                            "id": "root",
                            "component": "Button",
                            "child": "label",
                            "action": {
                                "event": {
                                    "name": "confirm",
                                    "context": { "approved": true }
                                }
                            }
                        },
                        { "id": "label", "component": "Text", "text": "Confirm" }
                    ]
                }
            }
        ])
    }

    fn client_message() -> A2uiClientMessage {
        A2uiClientMessage::parse(serde_json::json!({
            "version": "v0.9.1",
            "action": {
                "name": "confirm",
                "surfaceId": "confirm-order",
                "sourceComponentId": "root",
                "timestamp": "2026-07-24T10:00:00Z",
                "context": { "approved": true }
            }
        }))
        .unwrap_or_else(|error| panic!("valid client message: {error}"))
    }

    fn key() -> InteractionKey {
        InteractionKey {
            session_key: "main".into(),
            run_id: "run-1".into(),
            tool_call_id: "call-1".into(),
        }
    }

    #[test]
    fn validates_trusted_interaction() {
        let params = serde_json::json!({
            "messages": messages(),
            "_session_key": "main",
        });
        let interaction = validate_render_params(&params)
            .unwrap_or_else(|error| panic!("valid interaction: {error}"));
        assert_eq!(interaction.surface_id, "confirm-order");
    }

    #[test]
    fn rejects_untrusted_catalog_and_component() {
        let mut catalog_params = serde_json::json!({
            "messages": messages(),
        });
        catalog_params["messages"][0]["createSurface"]["catalogId"] =
            Value::String("https://attacker.invalid/catalog.json".into());
        assert!(validate_render_params(&catalog_params).is_err());

        let mut component_params = serde_json::json!({
            "messages": messages(),
        });
        component_params["messages"][1]["updateComponents"]["components"][0]["component"] =
            Value::String("ArbitraryHtml".into());
        assert!(validate_render_params(&component_params).is_err());
    }

    #[test]
    fn rejects_static_surface_that_cannot_answer() {
        let mut params = serde_json::json!({
            "messages": messages(),
        });
        params["messages"][1]["updateComponents"]["components"][0]
            .as_object_mut()
            .map(|component| component.remove("action"));
        assert!(validate_render_params(&params).is_err());
    }

    /// The surface element renders from the component whose id is `root`.
    /// Accepting an interaction without it produced a card that sat on
    /// "Loading surface..." until the tool timed out.
    #[test]
    fn rejects_an_interaction_without_the_root_component() {
        let mut params = serde_json::json!({
            "messages": messages(),
        });
        params["messages"][1]["updateComponents"]["components"][0]["id"] =
            Value::String("confirm-button".into());
        let error = validate_render_params(&params)
            .err()
            .unwrap_or_else(|| panic!("a surface without `root` must be rejected"));
        assert!(error.to_string().contains("\"id\": \"root\""));
    }

    #[test]
    fn reports_the_invalid_message_shape() {
        let params = serde_json::json!({
            "messages": [{
                "version": "v0.9.1",
                "surfaceUpdate": { "surfaceId": "confirm-order" }
            }],
        });
        let error = validate_render_params(&params)
            .err()
            .unwrap_or_else(|| panic!("non-standard message must be rejected"));
        let detail = error.to_string();
        assert!(detail.contains("A2UI message 1"));
        assert!(detail.contains("surfaceUpdate"));
        assert!(detail.contains("createSurface"));
    }

    #[test]
    fn reports_the_invalid_message_field() {
        let params = serde_json::json!({
            "messages": [{
                "version": "v0.9.1",
                "createSurface": { "surfaceId": "confirm-order" }
            }],
        });
        let error = validate_render_params(&params)
            .err()
            .unwrap_or_else(|| panic!("missing catalogId must be rejected"));
        let detail = error.to_string();
        assert!(detail.contains("A2UI message 1 `createSurface` is invalid"));
        assert!(detail.contains("catalogId"));
    }

    #[test]
    fn rejects_non_standard_nested_component_properties() {
        let mut params = serde_json::json!({
            "messages": messages(),
        });
        params["messages"][1]["updateComponents"]["components"][0]["properties"] =
            serde_json::json!({ "child": "label" });
        let error = validate_render_params(&params)
            .err()
            .unwrap_or_else(|| panic!("nested component properties must be rejected"));
        assert!(
            error
                .to_string()
                .contains("non-standard nested `properties`")
        );
    }

    fn media_params(component: &str, url: Value) -> Value {
        let mut params = serde_json::json!({
            "messages": messages(),
        });
        let mut media = serde_json::json!({ "id": "media", "component": component });
        if !url.is_null() {
            media["url"] = url;
        }
        params["messages"][1]["updateComponents"]["components"]
            .as_array_mut()
            .unwrap_or_else(|| panic!("components array"))
            .push(media);
        params
    }

    #[test]
    fn accepts_loadable_media_sources() {
        for component in ["Image", "Video", "AudioPlayer"] {
            for url in [
                serde_json::json!("https://cdn.example.com/asset.bin"),
                serde_json::json!("data:image/png;base64,iVBORw0KGgo="),
                serde_json::json!("/api/sessions/main/media/asset.bin"),
                serde_json::json!({ "path": "/asset" }),
            ] {
                let params = media_params(component, url.clone());
                validate_render_params(&params).unwrap_or_else(|error| {
                    panic!("{component} must accept {url}: {error}");
                });
            }
        }
    }

    #[test]
    fn rejects_media_without_a_loadable_url() {
        let missing = validate_render_params(&media_params("Image", Value::Null))
            .err()
            .unwrap_or_else(|| panic!("media without url must be rejected"));
        assert!(
            missing
                .to_string()
                .contains("is missing the required field `url`")
        );

        let blocked = validate_render_params(&media_params(
            "Video",
            serde_json::json!("http://cdn.example.com/clip.mp4"),
        ))
        .err()
        .unwrap_or_else(|| panic!("plain http media must be rejected"));
        assert!(
            blocked
                .to_string()
                .contains("content security policy only loads")
        );

        let empty = validate_render_params(&media_params("AudioPlayer", serde_json::json!("  ")))
            .err()
            .unwrap_or_else(|| panic!("empty media url must be rejected"));
        assert!(empty.to_string().contains("has an empty `url`"));
    }

    #[test]
    fn rejects_components_missing_catalog_required_fields() {
        let mut params = serde_json::json!({
            "messages": messages(),
        });
        params["messages"][1]["updateComponents"]["components"][1]
            .as_object_mut()
            .unwrap_or_else(|| panic!("component object"))
            .remove("text");
        let error = validate_render_params(&params)
            .err()
            .unwrap_or_else(|| panic!("Text without text must be rejected"));
        let detail = error.to_string();
        assert!(detail.contains("`label`"));
        assert!(detail.contains("missing the required field `text`"));
    }

    /// Build an interaction whose modal trigger optionally carries an action.
    fn modal_params(trigger_has_action: bool) -> Value {
        let trigger = if trigger_has_action {
            serde_json::json!({
                "id": "open-modal",
                "component": "Button",
                "child": "open-label",
                "action": { "event": { "name": "modal_opened", "context": {} } }
            })
        } else {
            serde_json::json!({ "id": "open-modal", "component": "Text", "text": "Open" })
        };
        let mut params = serde_json::json!({
            "messages": messages(),
        });
        let components = params["messages"][1]["updateComponents"]["components"]
            .as_array_mut()
            .unwrap_or_else(|| panic!("components array"));
        components.push(trigger);
        components.push(serde_json::json!({
            "id": "open-label", "component": "Text", "text": "Open"
        }));
        components.push(serde_json::json!({
            "id": "details", "component": "Text", "text": "Details"
        }));
        components.push(serde_json::json!({
            "id": "modal",
            "component": "Modal",
            "trigger": "open-modal",
            "content": "details"
        }));
        params
    }

    #[test]
    fn accepts_a_modal_whose_trigger_only_opens_the_dialog() {
        validate_render_params(&modal_params(false))
            .unwrap_or_else(|error| panic!("action-free modal trigger must be accepted: {error}"));
    }

    #[test]
    fn rejects_a_modal_trigger_that_also_sends_an_action() {
        let error = validate_render_params(&modal_params(true))
            .err()
            .unwrap_or_else(|| panic!("modal trigger with an action must be rejected"));
        let detail = error.to_string();
        assert!(detail.contains("uses trigger `open-modal` that also"));
        assert!(detail.contains("inside `content`"));
    }

    #[test]
    fn rejects_a_modal_trigger_that_is_not_declared() {
        let mut params = modal_params(false);
        let components = params["messages"][1]["updateComponents"]["components"]
            .as_array_mut()
            .unwrap_or_else(|| panic!("components array"));
        components.retain(|component| component["id"] != "open-modal");
        let error = validate_render_params(&params)
            .err()
            .unwrap_or_else(|| panic!("undeclared modal trigger must be rejected"));
        assert!(
            error
                .to_string()
                .contains("references the undeclared trigger component `open-modal`")
        );
    }

    #[test]
    fn tool_schema_documents_the_exact_protocol_without_one_of() {
        let tool = RenderA2uiTool::new(Arc::new(A2uiBroker::default()));
        let schema = tool.parameters_schema();
        let serialized = serde_json::to_string(&schema)
            .unwrap_or_else(|error| panic!("serialize tool schema: {error}"));
        assert!(!serialized.contains("oneOf"));
        assert_eq!(
            schema["properties"]["messages"]["items"]["properties"]["version"]["enum"][0],
            PROTOCOL_VERSION
        );
        assert_eq!(
            schema["properties"]["messages"]["items"]["properties"]["createSurface"]["properties"]
                ["catalogId"]["enum"][0],
            BASIC_CATALOG_ID
        );
        let description = schema["properties"]["messages"]["items"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(description.contains("Minimal valid `messages`"));
        assert!(description.contains("\"action\":{\"event\""));
    }

    /// The schema has to survive the OpenAI wire conversion. Without this the
    /// only signal that a field such as `options` lacks `items` is a refused
    /// tool call in a live session.
    #[test]
    fn tool_schema_converts_for_the_openai_wire_formats() {
        let tool = RenderA2uiTool::new(Arc::new(A2uiBroker::default()));
        let definition = serde_json::json!({
            "name": tool.name(),
            "description": tool.description(),
            "parameters": tool.parameters_schema(),
        });
        let tools = [definition];

        chelix_providers::openai_compat::to_openai_tools(&tools)
            .unwrap_or_else(|error| panic!("chat completions conversion: {error:#}"));
        chelix_providers::openai_compat::to_responses_api_tools(&tools)
            .unwrap_or_else(|error| panic!("responses conversion: {error:#}"));
    }

    #[test]
    fn tool_schema_exposes_media_component_fields() {
        let tool = RenderA2uiTool::new(Arc::new(A2uiBroker::default()));
        let schema = tool.parameters_schema();
        let component = &schema["properties"]["messages"]["items"]["properties"]["updateComponents"]
            ["properties"]["components"]["items"];
        for field in ["url", "description", "fit", "name", "options", "tabs"] {
            assert!(
                component["properties"][field].is_object(),
                "component schema must document `{field}`"
            );
        }
        let url_description = component["properties"]["url"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(url_description.contains("https:"));
        let component_description = component["description"].as_str().unwrap_or_default();
        assert!(component_description.contains("`AudioPlayer` -> `url`"));
    }

    #[tokio::test]
    async fn delivers_action_to_registered_waiter() {
        let broker = Arc::new(A2uiBroker::default());
        let waiting_broker = Arc::clone(&broker);
        let waiting_key = key();
        let waiter = tokio::spawn(async move { waiting_broker.wait(waiting_key).await });
        tokio::task::yield_now().await;
        broker
            .submit(key(), client_message())
            .unwrap_or_else(|error| panic!("submit action: {error}"));
        let received = waiter
            .await
            .unwrap_or_else(|error| panic!("join waiter: {error}"))
            .unwrap_or_else(|error| panic!("receive action: {error}"));
        assert_eq!(received.action.name, "confirm");
    }

    #[tokio::test]
    async fn buffers_action_that_arrives_before_waiter() {
        let broker = Arc::new(A2uiBroker::default());
        broker
            .submit(key(), client_message())
            .unwrap_or_else(|error| panic!("buffer action: {error}"));
        let received = broker
            .wait(key())
            .await
            .unwrap_or_else(|error| panic!("receive buffered action: {error}"));
        assert_eq!(received.action.surface_id, "confirm-order");
    }

    /// A stopped run drops the wait future. The waiter must then be gone, so a
    /// late action is refused instead of resolving an interaction nobody is
    /// listening to.
    #[tokio::test]
    async fn abandoning_the_wait_removes_the_waiter() {
        let broker = Arc::new(A2uiBroker::default());
        let waiting_broker = Arc::clone(&broker);
        let waiter = tokio::spawn(async move { waiting_broker.wait(key()).await });
        tokio::task::yield_now().await;
        waiter.abort();
        assert!(waiter.await.is_err());
        assert!(matches!(
            broker.submit(key(), client_message()),
            Err(BrokerSubmitError::Duplicate)
        ));
    }

    #[tokio::test]
    async fn rejects_duplicate_action_after_delivery() {
        let broker = Arc::new(A2uiBroker::default());
        broker
            .submit(key(), client_message())
            .unwrap_or_else(|error| panic!("buffer action: {error}"));
        broker
            .wait(key())
            .await
            .unwrap_or_else(|error| panic!("receive buffered action: {error}"));
        assert!(matches!(
            broker.submit(key(), client_message()),
            Err(BrokerSubmitError::Duplicate)
        ));
    }
}
