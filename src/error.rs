use std::process::ExitCode;

use serde::Serialize;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, EasyMusicError>;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidArguments,
    NoResults,
    AmbiguousSelection,
    UpstreamApi,
    AudioSource,
    Transcode,
    DependencyMissing,
    Interrupted,
    Io,
}

impl ErrorCode {
    pub fn exit_code(self) -> u8 {
        match self {
            Self::InvalidArguments => 2,
            Self::NoResults => 3,
            Self::AmbiguousSelection => 4,
            Self::UpstreamApi => 5,
            Self::AudioSource => 6,
            Self::Transcode => 7,
            Self::DependencyMissing => 8,
            Self::Interrupted => 130,
            Self::Io => 1,
        }
    }

    pub fn as_exit_code(self) -> ExitCode {
        ExitCode::from(self.exit_code())
    }
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct EasyMusicError {
    pub code: ErrorCode,
    pub message: String,
}

impl EasyMusicError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArguments, message)
    }

    pub fn upstream(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::UpstreamApi, message)
    }

    pub fn source(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::AudioSource, message)
    }

    pub fn transcode(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Transcode, message)
    }
}

impl From<std::io::Error> for EasyMusicError {
    fn from(value: std::io::Error) -> Self {
        Self::new(ErrorCode::Io, value.to_string())
    }
}

impl From<reqwest::Error> for EasyMusicError {
    fn from(value: reqwest::Error) -> Self {
        Self::upstream(value.to_string())
    }
}

impl From<url::ParseError> for EasyMusicError {
    fn from(value: url::ParseError) -> Self {
        Self::source(value.to_string())
    }
}
