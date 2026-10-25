//! Call bus between the Chelix host and WASM plugins.

pub mod address;
pub mod info;
pub mod jsonrpc;

#[cfg(feature = "host")]
pub mod activation;
#[cfg(feature = "host")]
pub mod bus;
#[cfg(feature = "host")]
pub mod compat;
#[cfg(feature = "host")]
pub mod document;
#[cfg(feature = "host")]
pub mod events;
#[cfg(feature = "host")]
pub mod metrics;
#[cfg(feature = "host")]
pub mod registry;
#[cfg(feature = "host")]
pub mod resources;
#[cfg(feature = "host")]
pub mod schema;
#[cfg(feature = "host")]
mod wasm;

#[cfg(feature = "guest")]
mod plugin_macro;
#[cfg(feature = "guest")]
pub mod guest;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
mod suite;

#[cfg(feature = "host")]
pub use bus::{BusOptions, CallBus, StartError};
#[cfg(feature = "host")]
pub use jsonrpc::RpcError;
pub use info::{PluginInfo, PluginStatus, ProviderInfo};
