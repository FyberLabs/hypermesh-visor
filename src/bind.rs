use std::fmt;
use std::net::SocketAddr;

#[derive(Debug, PartialEq, Eq)]
pub enum ListenError {
    Invalid(String),
    NotLoopback,
}

impl fmt::Display for ListenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(value) => write!(f, "invalid listen address: {value}"),
            Self::NotLoopback => write!(f, "listen address must be loopback"),
        }
    }
}

impl std::error::Error for ListenError {}

pub fn parse_listen(value: &str) -> Result<SocketAddr, ListenError> {
    let addr: SocketAddr = value
        .parse()
        .map_err(|_| ListenError::Invalid(value.to_string()))?;
    if !addr.ip().is_loopback() {
        return Err(ListenError::NotLoopback);
    }
    Ok(addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_loopback() {
        assert_eq!(
            parse_listen("127.0.0.1:9847").unwrap().to_string(),
            "127.0.0.1:9847"
        );
        assert!(parse_listen("[::1]:9847").unwrap().ip().is_loopback());
        assert!(parse_listen("127.0.0.1:0").is_ok());
    }

    #[test]
    fn rejects_non_loopback() {
        assert_eq!(parse_listen("0.0.0.0:9847"), Err(ListenError::NotLoopback));
        assert_eq!(
            parse_listen("192.168.1.10:9847"),
            Err(ListenError::NotLoopback)
        );
        assert!(matches!(
            parse_listen("localhost:9847"),
            Err(ListenError::Invalid(_))
        ));
    }
}
