//! Metric recording for `duckduckgo_search` executions.

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, tools as tools_metrics};

/// Record one tool execution outcome.
#[cfg(feature = "metrics")]
pub(crate) fn record_execution(tool: &str, success: bool) {
    if success {
        counter!(
            tools_metrics::EXECUTIONS_TOTAL,
            labels::TOOL => tool.to_string(),
            labels::SUCCESS => "true".to_string()
        )
        .increment(1);
    } else {
        counter!(
            tools_metrics::EXECUTION_ERRORS_TOTAL,
            labels::TOOL => tool.to_string()
        )
        .increment(1);
    }
}

#[cfg(not(feature = "metrics"))]
pub(crate) fn record_execution(_tool: &str, _success: bool) {}
