/// Typed request handled by exactly one registered closure.
pub trait Procedure: Send + 'static {
    type Output: Send + 'static;
    type Error: std::error::Error + Send + Sync + 'static;
}

/// Typed event delivered to zero or more subscribers.
pub trait Event: Clone + Send + 'static {}

/// How `publish` delivers one event.
pub enum Delivery {
    /// Wait for every subscriber. Subscriber errors return to the publisher.
    Wait,
    /// Enqueue the event. The publisher does not wait.
    Queue,
    /// A slow receiver may lose the event. The publisher selects loss with this variant.
    Detach,
}
