use std::fmt;

#[derive(Debug)]
pub struct ServiceError {
    pub code: String,
    pub message: String,
}

impl ServiceError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn code(error: &anyhow::Error) -> &str {
        error
            .downcast_ref::<Self>()
            .map_or("INTERNAL_ERROR", |error| &error.code)
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ServiceError {}

pub fn fail<T>(code: &str, message: impl Into<String>) -> anyhow::Result<T> {
    Err(ServiceError::new(code, message).into())
}
