use std::{future::Future, pin::Pin};

use futures::{FutureExt, StreamExt};

use super::{CompletionResponse, StreamEvent};

pub(crate) fn response_stream<'a>(
    response: impl Future<Output = anyhow::Result<CompletionResponse>> + Send + 'a,
) -> Pin<Box<dyn tokio_stream::Stream<Item = StreamEvent> + Send + 'a>> {
    Box::pin(response.into_stream().flat_map(|response| {
        let mut events = Vec::new();
        match response {
            Ok(response) => {
                if let Some(text) = response.text {
                    events.push(StreamEvent::Delta(text));
                }
                for (index, call) in response.tool_calls.into_iter().enumerate() {
                    events.push(StreamEvent::ToolCallStart {
                        id: call.id,
                        name: call.name,
                        index,
                    });
                    events.push(StreamEvent::ToolCallArgumentsDelta {
                        index,
                        delta: call.arguments.to_string(),
                    });
                    events.push(StreamEvent::ToolCallComplete { index });
                }
                events.push(StreamEvent::Done(response.usage));
            },
            Err(error) => events.push(StreamEvent::Error(error.to_string())),
        }
        tokio_stream::iter(events)
    }))
}
