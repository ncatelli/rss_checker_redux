use std::ffi::OsString;

use url::ParseError;

#[derive(Debug)]
pub enum ErrorKind {
    InvalidUrl { reason: ParseError, url: String },
    DuplicateFeed(String),
    IoErr(std::io::Error),
    InvalidFilename(OsString),
    ReqwestErr(reqwest::Error),
    RssErr(rss::Error),
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateFeed(feed_name) => {
                write!(f, "feed {} is defined more than once", feed_name)
            }
            Self::InvalidUrl { reason, url } => write!(f, "{} for {}", reason, url),
            Self::IoErr(err) => write!(f, "{}", err),
            Self::InvalidFilename(repr) => {
                write!(f, "filename must be representable as utf-8: {:?}", repr)
            }
            Self::ReqwestErr(err) => write!(f, "{}", err),
            Self::RssErr(err) => write!(f, "{}", err),
        }
    }
}

#[derive(Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub data: Option<String>,
}

impl Error {
    pub fn new(kind: ErrorKind) -> Self {
        Self { kind, data: None }
    }

    /// Allows a caller to enrich an error with a string signifying additional
    /// information about the error.
    pub fn with_data_mut<S: AsRef<str>>(&mut self, data: S) {
        let data = data.as_ref().to_string();

        self.data = Some(data)
    }

    /// Allows a caller to enrich an error with a string signifying additional
    /// information about the error.
    #[allow(unused)]
    pub fn with_data<S: AsRef<str>>(mut self, data: S) -> Self {
        self.with_data_mut(data);
        self
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.data {
            Some(ctx) => write!(f, "{}: {}", &self.kind, ctx),
            None => write!(f, "{}", &self.kind),
        }
    }
}
