use chelix_service_traits::ServiceError;

pub(super) fn writer_error(error: chelix_config_service::Error) -> ServiceError {
    match error {
        chelix_config_service::Error::Validation(message) => ServiceError::message(message),
        other => ServiceError::message(other),
    }
}
