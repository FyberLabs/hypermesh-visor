use std::fmt;

#[derive(Debug)]
pub enum McpError {
    Message(String),
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for McpError {}

impl McpError {
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}

