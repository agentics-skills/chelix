//! Metric and span names written by the bus.

pub const CALLS_TOTAL: &str = "chelix_call_bus_calls_total";
pub const CALL_DURATION: &str = "chelix_call_bus_call_duration_seconds";
pub const PLUGIN_TRANSITIONS: &str = "chelix_call_bus_plugin_transitions_total";

pub const SPAN_CALL: &str = "call_bus.call";
pub const SPAN_INVOKE: &str = "call_bus.invoke";
pub const SPAN_PUBLISH: &str = "call_bus.publish";
pub const SPAN_PLUGIN: &str = "call_bus.plugin";

#[cfg(feature = "metrics")]
pub fn record_call(op: &str, address: &str, code: i64, seconds: f64) {
    metrics::counter!(CALLS_TOTAL, "op" => op.to_owned(), "address" => address.to_owned(), "code" => code.to_string()).increment(1);
    metrics::histogram!(CALL_DURATION, "op" => op.to_owned(), "address" => address.to_owned()).record(seconds);
}

#[cfg(not(feature = "metrics"))]
pub fn record_call(_op: &str, _address: &str, _code: i64, _seconds: f64) {}

#[cfg(feature = "metrics")]
pub fn record_transition(plugin: &str, status: &str) {
    metrics::counter!(PLUGIN_TRANSITIONS, "plugin" => plugin.to_owned(), "status" => status.to_owned()).increment(1);
}

#[cfg(not(feature = "metrics"))]
pub fn record_transition(_plugin: &str, _status: &str) {}
