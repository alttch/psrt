//! PubSubRT server, cli and client library
//!
//! <https://github.com/alttch/psrt>
//!
//! Rust client example:
//!
//! ```rust,ignore
#![doc = include_str!("../examples/pubsub.rs")]
//! ```
//!
//! Python client (sync): <https://github.com/alttch/psrt-py>
// TODO UDP encryptions aes-128-gcm & aes-256
// TODO permanent subs and udp push
// TODO data queue sizes in stats
// TODO priorities
// TODO web sockets
// TODO admin area
// TODO keep subscribed topics as chunks
use tokio_rustls::webpki;

use std::fmt;
use std::time::{Duration, Instant};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const AUTHOR: &str = "(c) 2021 Bohemia Automation / Altertech";

pub const DEFAULT_PRIORITY: u8 = 0x7F;

pub const CONTROL_HEADER: [u8; 2] = [0xEE, 0xAA];
pub const DATA_HEADER: [u8; 2] = [0xEE, 0xAB];

pub const PROTOCOL_VERSION: u16 = 1;

pub const OP_NOP: u8 = 0x00;
pub const RESPONSE_OK: u8 = 0x01;
pub const RESPONSE_OK_WAITING: u8 = 0x02;
pub const RESPONSE_NOT_REQUIRED: u8 = 0xE0;
pub const RESPONSE_ERR: u8 = 0xFF;
pub const RESPONSE_ERR_ACCESS: u8 = 0xFE;

pub const OP_PUBLISH: u8 = 0x01;
pub const OP_PUBLISH_REPL: u8 = 0x11;
pub const OP_PUBLISH_NO_ACK: u8 = 0x21; // for UDP only
pub const OP_SUBSCRIBE: u8 = 0x02;
pub const OP_UNSUBSCRIBE: u8 = 0x03;
pub const OP_BYE: u8 = 0xFF;

pub const COMM_INSECURE: u8 = 0x00;
pub const COMM_TLS: u8 = 0x01;

pub const AUTH_LOGIN_PASS: u8 = 0x00;
pub const AUTH_KEY_PLAIN: u8 = 0x01;
pub const AUTH_KEY_AES128_: u8 = 0x02;
pub const AUTH_KEY_AES256: u8 = 0x03;

#[inline]
pub fn reduce_timeout(timeout: Duration, op_start: Instant) -> Duration {
    let spent = op_start.elapsed();
    if spent > timeout {
        Duration::from_secs(0)
    } else {
        timeout - spent
    }
}
#[derive(Debug, Clone)]
pub struct Message {
    priority: u8,
    topic: String,
    data: Vec<u8>,
}

impl Message {
    #[inline]
    pub fn topic(&self) -> &str {
        &self.topic
    }
    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
    /// # Errors
    ///
    /// Will return Err if data is unable to be parsed as utf8
    #[inline]
    pub fn data_as_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.data)
    }
    /// # Errors
    ///
    /// Will return Err on buffer parse errors
    pub fn from_buf(buf: &mut Vec<u8>, priority: u8) -> Result<Self, Error> {
        let npos = buf
            .iter()
            .position(|n| *n == 0)
            .ok_or_else(|| Error::invalid_data("Invalid msg frame"))?;
        let data = buf.split_off(npos + 1);
        let topic = std::str::from_utf8(&buf[..buf.len() - 1])?;
        Ok(Self {
            priority,
            topic: topic.to_owned(),
            data,
        })
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    AccessDenied,
    InvalidData,
    Io,
    Eof,
    Timeout,
    Internal,
    TryLater,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                ErrorKind::AccessDenied => "Access denied",
                ErrorKind::InvalidData => "Invalid data",
                ErrorKind::Io => "I/O error",
                ErrorKind::Eof => "EOF",
                ErrorKind::Internal => "Internal error",
                ErrorKind::Timeout => "Timeout",
                ErrorKind::TryLater => "Will try later",
            }
        )
    }
}

#[derive(Debug, Clone)]
pub struct Error {
    kind: ErrorKind,
    message: Option<String>,
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        if e.kind() == std::io::ErrorKind::UnexpectedEof
            || e.kind() == std::io::ErrorKind::BrokenPipe
        {
            Error::eof(e)
        } else {
            Error::io(e)
        }
    }
}

impl From<serde_yaml::Error> for Error {
    fn from(e: serde_yaml::Error) -> Error {
        Error::invalid_data(e)
    }
}

impl From<std::str::Utf8Error> for Error {
    fn from(e: std::str::Utf8Error) -> Error {
        Error::invalid_data(e)
    }
}

impl From<tokio::time::error::Elapsed> for Error {
    fn from(e: tokio::time::error::Elapsed) -> Error {
        Error::timeout(e)
    }
}

impl From<webpki::Error> for Error {
    fn from(e: webpki::Error) -> Error {
        Error::io(e)
    }
}

impl Error {
    pub fn access(message: impl fmt::Display) -> Self {
        Self {
            kind: ErrorKind::AccessDenied,
            message: Some(message.to_string()),
        }
    }
    pub fn io(message: impl fmt::Display) -> Self {
        Self {
            kind: ErrorKind::Io,
            message: Some(message.to_string()),
        }
    }
    pub fn eof(message: impl fmt::Display) -> Self {
        Self {
            kind: ErrorKind::Eof,
            message: Some(message.to_string()),
        }
    }
    pub fn invalid_data(message: impl fmt::Display) -> Self {
        Self {
            kind: ErrorKind::InvalidData,
            message: Some(message.to_string()),
        }
    }
    pub fn internal(message: impl fmt::Display) -> Self {
        Self {
            kind: ErrorKind::Internal,
            message: Some(message.to_string()),
        }
    }
    pub fn later() -> Self {
        Self {
            kind: ErrorKind::TryLater,
            message: None,
        }
    }
    pub fn timeout(message: impl fmt::Display) -> Self {
        Self {
            kind: ErrorKind::Timeout,
            message: Some(message.to_string()),
        }
    }
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ref message) = self.message {
            write!(f, "{}: {}", self.kind, message)
        } else {
            write!(f, "{}", self.kind)
        }
    }
}

#[cfg(feature = "server")]
pub mod acl;
pub mod client;
pub mod comm;
#[cfg(feature = "server")]
pub mod passwords;
#[cfg(feature = "server")]
pub mod pubsub;
#[cfg(feature = "cluster")]
pub mod replication;
#[cfg(any(feature = "server", feature = "cli"))]
pub mod token;
