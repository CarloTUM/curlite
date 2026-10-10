//! A small blocking HTTP client on top of libcurl.
//!
//! `curlite` is for programs that call HTTP services and want to stay small:
//! no async runtime, one libcurl handle per thread with connection reuse,
//! request bodies held in memory once, and `multipart/form-data` in both
//! directions.
//!
//! ```no_run
//! use curlite::Multipart;
//!
//! let response = curlite::get("https://httpbin.org/get")
//!     .query("page", "2")
//!     .send()?;
//! println!("{} {}", response.status(), response.text()?);
//!
//! let response = curlite::post("https://httpbin.org/post")
//!     .multipart(
//!         Multipart::new()
//!             .text("description", "report")
//!             .file("file", "report.pdf", mime::APPLICATION_PDF, std::fs::read("report.pdf")?),
//!     )
//!     .send()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Bodies are set with [`Request::body`] (raw bytes with a MIME type),
//! [`Request::form`] (`application/x-www-form-urlencoded`) or
//! [`Request::multipart`]. Multipart responses are split with
//! [`Response::parts`]. The [`params`] module adds a parameter model in
//! which the body format follows from the parameters.
//!
//! # TLS
//!
//! TLS comes from libcurl. By default the system libcurl is linked
//! dynamically, with whatever TLS library it was built against. The
//! `static-curl`, `static-ssl` and `rustls` features are passed through to
//! the [`curl`] crate to build and link libcurl statically instead.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod body;
mod error;
pub mod params;
mod request;
mod response;
mod transport;

pub use body::Multipart;
pub use error::{Error, Result};
pub use request::{Request, delete, get, head, patch, post, put};
pub use response::{Response, ResponsePart};

pub use bytes::Bytes;
pub use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
pub use mime::Mime;
