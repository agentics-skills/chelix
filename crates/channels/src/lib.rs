//! Channel plugin system.
//!
//! Each channel (Telegram, WhatsApp, etc.) implements the
//! ChannelPlugin trait with sub-traits for config, auth, inbound/outbound
//! messaging, status, and gateway lifecycle.

pub mod commands;
pub mod config_view;
pub mod contract;
pub mod error;
pub mod gating;
pub mod media_download;
pub mod message_log;
pub mod otp;
pub mod plugin;
pub mod registry;
pub mod store;

pub use {
    config_view::ChannelConfigView,
    error::{Error, Result},
    media_download::{InboundMediaDownloader, InboundMediaSource},
    plugin::{
        ButtonRow, ButtonStyle, ChannelAttachment, ChannelCapabilities, ChannelDescriptor,
        ChannelDocumentFile, ChannelEvent, ChannelEventSink, ChannelHealthSnapshot,
        ChannelMessageKind, ChannelMessageMeta, ChannelOtpProvider, ChannelOutbound, ChannelPlugin,
        ChannelReplyTarget, ChannelStatus, ChannelStreamOutbound, ChannelType, InboundMode,
        InteractiveButton, InteractiveMessage, SavedChannelFile, StreamEvent, StreamReceiver,
        StreamSender, resolve_session_channel_binding, web_session_channel_binding,
    },
    registry::{ChannelRegistry, RegistryOutboundRouter},
};
