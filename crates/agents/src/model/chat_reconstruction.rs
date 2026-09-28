/// Streaming conversion of ordered canonical records into provider messages.
pub struct ChatReconstruction {
    messages: Vec<ChatMessage>,
    pending_tool_call_ids: std::collections::HashSet<String>,
    provider_segments: Vec<chelix_common::ProviderSegmentMaterializer>,
    replayed_segment_ids: std::collections::HashSet<chelix_common::ProviderSegmentId>,
    filter_orphan_tool_results: bool,
}

impl ChatReconstruction {
    pub fn new(
        filter_orphan_tool_results: bool,
        replayed_segment_ids: std::collections::HashSet<chelix_common::ProviderSegmentId>,
    ) -> Self {
        Self {
            messages: Vec::new(),
            pending_tool_call_ids: std::collections::HashSet::new(),
            provider_segments: Vec::new(),
            replayed_segment_ids,
            filter_orphan_tool_results,
        }
    }

    pub fn push(
        &mut self,
        index: usize,
        val: &serde_json::Value,
    ) -> Result<(), ChatMessageConversionError> {
        let Some(role) = val["role"].as_str() else {
            tracing::warn!(index, "skipping message with missing/invalid role");
            return Ok(());
        };
        match role {
            "system" => {
                let content = val["content"].as_str().unwrap_or("").to_string();
                self.messages.push(ChatMessage::system(content));
            },
            "user" => self.push_user(val),
            "assistant" => self.push_assistant(index, val)?,
            "tool" => self.push_tool(val),
            "tool_lifecycle" => self.push_tool_lifecycle(index, val)?,
            "notice" => {},
            "provider_update" => self.push_provider_update(index, val)?,
            "provider_segment_close" => self.push_provider_close(index, val)?,
            "checkpoint" => {
                let summary = val["summary"].as_str().unwrap_or("");
                self.messages.push(ChatMessage::user(format!(
                    "<conversation-summary>\n{summary}\n</conversation-summary>"
                )));
            },
            other => {
                return Err(ChatMessageConversionError::UnsupportedRole {
                    message_index: index,
                    role: other.to_string(),
                });
            },
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<Vec<ChatMessage>, ChatMessageConversionError> {
        append_replayed_provider_segments(
            &mut self.messages,
            &self.provider_segments,
            &mut self.replayed_segment_ids,
        );
        super::aborted_calls::ensure_tool_call_results_present(&mut self.messages);
        Ok(self.messages)
    }

    fn push_user(&mut self, val: &serde_json::Value) {
        let sender_name = val
            .get("channel")
            .and_then(|channel| {
                channel["sender_name"]
                    .as_str()
                    .or_else(|| channel["username"].as_str())
            })
            .or_else(|| val["name"].as_str())
            .map(str::to_string);
        let document_context = val["documents"].as_array().and_then(|documents| {
            let mut sections = Vec::new();
            for document in documents {
                let Some(display_name) = document["display_name"].as_str() else {
                    continue;
                };
                let Some(mime_type) = document["mime_type"].as_str() else {
                    continue;
                };
                let Some(media_ref) = document["media_ref"].as_str() else {
                    continue;
                };
                let absolute_path = document_absolute_path_from_media_ref(media_ref);
                sections.push(format!(
                    "filename: {display_name}\n\
                     mime_type: {mime_type}\n\
                     local_path: {absolute_path}\n\
                     media_ref: {media_ref}"
                ));
            }
            if sections.is_empty() {
                None
            } else {
                let mut rendered = vec!["[Inbound documents available]".to_string()];
                rendered.extend(sections);
                Some(rendered.join("\n\n"))
            }
        });
        if let Some(text) = val["content"].as_str() {
            let content = if let Some(ref document_context) = document_context {
                if text.trim().is_empty() {
                    document_context.clone()
                } else {
                    format!("{text}\n\n{document_context}")
                }
            } else {
                text.to_string()
            };
            self.messages.push(ChatMessage::User {
                content: UserContent::Text(content),
                name: sender_name,
            });
        } else if let Some(blocks) = val["content"].as_array() {
            let mut parts: Vec<ContentPart> = blocks
                .iter()
                .filter_map(|block| {
                    let block_type = block["type"].as_str()?;
                    match block_type {
                        "text" => Some(ContentPart::Text(block["text"].as_str()?.to_string())),
                        "image_url" => {
                            let url = block["image_url"]["url"].as_str()?;
                            let (media_type, data) = parse_data_uri(url)?;
                            Some(ContentPart::Image {
                                media_type: media_type.to_string(),
                                data: data.to_string(),
                            })
                        },
                        _ => None,
                    }
                })
                .collect();
            if let Some(document_context) = document_context {
                if let Some(ContentPart::Text(text)) = parts
                    .iter_mut()
                    .find(|part| matches!(part, ContentPart::Text(_)))
                {
                    if !text.trim().is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(&document_context);
                } else {
                    parts.insert(0, ContentPart::Text(document_context));
                }
            }
            self.messages.push(ChatMessage::User {
                content: UserContent::Multimodal(parts),
                name: sender_name,
            });
        } else {
            self.messages.push(ChatMessage::User {
                content: UserContent::Text(document_context.unwrap_or_default()),
                name: sender_name,
            });
        }
    }

    fn push_assistant(
        &mut self,
        index: usize,
        val: &serde_json::Value,
    ) -> Result<(), ChatMessageConversionError> {
        let content = val["content"].as_str().map(str::to_string);
        let reasoning = decode_reasoning(val.get("reasoning"), index)?;
        let provider_items = match val.get("providerItems") {
            None => Vec::new(),
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .enumerate()
                .map(|(item_index, item)| {
                    serde_json::from_value(item.clone()).map_err(|source| {
                        ChatMessageConversionError::ProviderItem {
                            message_index: index,
                            item_index,
                            source,
                        }
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => {
                return Err(ChatMessageConversionError::ProviderItemsCollection {
                    message_index: index,
                });
            },
        };
        let segment_id = val.get("segmentId").and_then(|value| {
            serde_json::from_value::<chelix_common::ProviderSegmentId>(value.clone()).ok()
        });
        let tool_calls: Vec<ToolCall> = val["tool_calls"]
            .as_array()
            .map(|calls| {
                calls
                    .iter()
                    .filter_map(|call| {
                        let id = call["id"].as_str()?.to_string();
                        let name = call["function"]["name"].as_str()?.to_string();
                        let decoded = decode_tool_call_arguments_with_diagnostic(
                            call["function"].get("arguments"),
                        );
                        Some(ToolCall {
                            id,
                            name,
                            arguments: decoded.arguments,
                            argument_diagnostic: decoded.diagnostic,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        for call in &tool_calls {
            self.pending_tool_call_ids.insert(call.id.clone());
        }
        self.messages.push(ChatMessage::Assistant {
            content,
            tool_calls,
            reasoning,
            provider_items,
            segment_id,
        });
        Ok(())
    }

    fn push_tool(&mut self, val: &serde_json::Value) {
        let tool_call_id = val["tool_call_id"].as_str().unwrap_or("").to_string();
        let has_matching_assistant = self.pending_tool_call_ids.remove(&tool_call_id);
        if self.filter_orphan_tool_results && !has_matching_assistant {
            tracing::debug!(tool_call_id, "skipping orphan tool message");
            return;
        }
        let content = if let Some(text) = val["content"].as_str() {
            text.to_string()
        } else {
            val["content"].to_string()
        };
        self.messages.push(ChatMessage::tool(tool_call_id, content));
    }

    fn push_tool_lifecycle(
        &mut self,
        index: usize,
        val: &serde_json::Value,
    ) -> Result<(), ChatMessageConversionError> {
        let lifecycle =
            serde_json::from_value::<ToolLifecycleEvent>(val.clone()).map_err(|source| {
                ChatMessageConversionError::ToolLifecycle {
                    message_index: index,
                    source,
                }
            })?;
        let content = match lifecycle.update {
            ToolLifecycleUpdate::Completed { result, error, .. } => result.unwrap_or_else(|| {
                error.map_or_else(String::new, |error| format!("Error: {error}"))
            }),
            ToolLifecycleUpdate::Rejected { result, .. } => result,
            ToolLifecycleUpdate::Cancelled { reason, .. } => {
                format!("Tool call cancelled: {reason}")
            },
            _ => return Ok(()),
        };
        let has_matching_assistant = self.pending_tool_call_ids.remove(&lifecycle.tool_call_id);
        if self.filter_orphan_tool_results && !has_matching_assistant {
            tracing::debug!(
                tool_call_id = lifecycle.tool_call_id,
                "skipping orphan terminal tool lifecycle message"
            );
            return Ok(());
        }
        self.messages
            .push(ChatMessage::tool(lifecycle.tool_call_id, content));
        Ok(())
    }

    fn push_provider_update(
        &mut self,
        index: usize,
        val: &serde_json::Value,
    ) -> Result<(), ChatMessageConversionError> {
        let update = serde_json::from_value::<chelix_common::ProviderItemUpdate>(val.clone())
            .map_err(|source| ChatMessageConversionError::ProviderUpdate {
                message_index: index,
                source,
            })?;
        replayed_segment(&mut self.provider_segments, &update.segment_id)
            .apply_update(&update)
            .map_err(|source| ChatMessageConversionError::ProviderSegmentReplay {
                message_index: index,
                source,
            })?;
        Ok(())
    }

    fn push_provider_close(
        &mut self,
        index: usize,
        val: &serde_json::Value,
    ) -> Result<(), ChatMessageConversionError> {
        let close =
            serde_json::from_value::<PersistedSegmentClose>(val.clone()).map_err(|source| {
                ChatMessageConversionError::ProviderSegmentClose {
                    message_index: index,
                    source,
                }
            })?;
        replayed_segment(&mut self.provider_segments, &close.segment_id)
            .close(close.outcome)
            .map_err(|source| ChatMessageConversionError::ProviderSegmentReplay {
                message_index: index,
                source,
            })?;
        flush_replayed_segment(
            &mut self.messages,
            &self.provider_segments,
            &close.segment_id,
            &mut self.replayed_segment_ids,
        );
        Ok(())
    }
}
