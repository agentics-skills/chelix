//! Typed Linkup search API request and response shapes.

use serde::{Deserialize, Serialize};

/// Request body for `POST /v1/search`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LinkupSearchRequest<'a> {
    pub(crate) q: &'a str,
    pub(crate) depth: &'static str,
    pub(crate) output_type: &'static str,
    pub(crate) include_images: bool,
    pub(crate) max_results: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) include_domains: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) from_date: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) to_date: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LinkupResultItem {
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) content: Option<String>,
    pub(crate) snippet: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LinkupSourceItem {
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) snippet: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LinkupApiResponse {
    pub(crate) results: Option<Vec<LinkupResultItem>>,
    pub(crate) answer: Option<String>,
    pub(crate) sources: Option<Vec<LinkupSourceItem>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LinkupErrorDetail {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LinkupApiErrorBody {
    pub(crate) message: Option<String>,
    pub(crate) details: Option<Vec<LinkupErrorDetail>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LinkupApiErrorEnvelope {
    pub(crate) error: Option<LinkupApiErrorBody>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LinkupSearchError {
    pub(crate) error: LinkupSearchErrorBody,
}

#[derive(Debug, Serialize)]
pub(crate) struct LinkupSearchErrorBody {
    pub(crate) code: u16,
    pub(crate) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) details: Option<Vec<LinkupErrorDetail>>,
}
