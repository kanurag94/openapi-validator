//! The request and response the caller hands in.
//!
//! Both are plain structs of borrowed slices so a proxy can describe the
//! message it already holds without copying it.

/// An HTTP request as received.
#[derive(Debug, Clone, Copy, Default)]
pub struct Request<'a> {
    /// The method token as sent, such as `GET`. Methods are case-sensitive
    /// (RFC 9110 §9.1).
    pub method: &'a str,
    /// The path as sent, still percent-encoded, without the query string.
    pub path: &'a str,
    /// The raw query string after `?`, without the `?`. Empty when absent.
    pub query: &'a str,
    /// Header lines in arrival order, repeats kept.
    pub headers: &'a [(&'a str, &'a str)],
    /// The body bytes. Empty when there is no body.
    pub body: &'a [u8],
}

/// An HTTP response as received from the upstream service.
#[derive(Debug, Clone, Copy, Default)]
pub struct Response<'a> {
    pub status: u16,
    /// Header lines in arrival order, repeats kept.
    pub headers: &'a [(&'a str, &'a str)],
    pub body: &'a [u8],
}
