/// Registration failure.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegError {
    /// A handler for this procedure type is already registered.
    #[error("procedure handler already registered")]
    Duplicate,
    /// `seal` found a required procedure without a handler.
    #[error("required procedure has no handler")]
    Missing,
    /// The registry is sealed.
    #[error("call bus is sealed")]
    Sealed,
}

/// Failure of `CallBus::call`.
#[derive(Debug, thiserror::Error)]
pub enum CallError<E>
where
    E: std::error::Error + 'static,
{
    /// `call` happened before a successful `seal`.
    #[error("call bus is not sealed")]
    NotSealed,
    /// No handler is registered for this procedure.
    #[error("call bus has no handler for this procedure")]
    Missing,
    /// The handler returned an error.
    #[error(transparent)]
    Failed(E),
}

/// Failure of `CallBus::publish`.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// `publish` happened before a successful `seal`.
    #[error("call bus is not sealed")]
    NotSealed,
    /// `publish` happened after `close` began.
    #[error("call bus is closed")]
    Closed,
    /// The queue limit rejected the event. No subscriber accepted it.
    #[error("call bus queue is full")]
    Full,
    /// One or more `Wait` subscribers failed. Successful subscribers are not retried.
    #[error("call bus subscriber failed")]
    Subscriber(Vec<Box<dyn std::error::Error + Send + Sync>>),
}
