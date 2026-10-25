//! Subscriber queues for `queue` delivery.

use std::collections::{HashMap, VecDeque};

use serde_json::Value;

use crate::jsonrpc::RpcError;

#[derive(Debug)]
pub struct SubscriberQueue {
    pub events: VecDeque<QueuedEvent>,
}

#[derive(Debug, Clone)]
pub struct QueuedEvent {
    pub method: String,
    pub payload: Value,
}

#[derive(Debug, Default)]
pub struct EventQueues {
    pub by_plugin: HashMap<String, SubscriberQueue>,
}

impl EventQueues {
    pub fn push(&mut self, plugin: &str, event: QueuedEvent) {
        self.by_plugin
            .entry(plugin.to_owned())
            .or_insert_with(|| SubscriberQueue {
                events: VecDeque::new(),
            })
            .events
            .push_back(event);
    }

    pub fn pop(&mut self, plugin: &str) -> Option<QueuedEvent> {
        self.by_plugin.get_mut(plugin).and_then(|queue| queue.events.pop_front())
    }

    pub fn clear(&mut self, plugin: &str) -> usize {
        self.by_plugin
            .remove(plugin)
            .map(|queue| queue.events.len())
            .unwrap_or(0)
    }
}

#[derive(Debug)]
pub enum PublishError {
    Undeclared,
    Invalid(String),
    Closed,
    Subscriber(Vec<RpcError>),
}
