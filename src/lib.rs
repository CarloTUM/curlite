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
//! [`Request::body_reader`] (streamed from a file or other reader),
//! [`Request::form`] (`application/x-www-form-urlencoded`) or
//! [`Request::multipart`]. Responses are read into memory by
//! [`Request::send`], streamed to a writer by [`Request::send_to`] or read
//! on demand through [`Request::send_reader`]. Multipart responses are
//! split with [`Response::parts`]. A [`Client`] holds headers, timeouts,
//! proxy and TLS settings shared by several requests.
//!
//! # Features
//!
//! * `json`: [`Request::json`] and [`Response::json`] via `serde_json`.
//! * `log`: libcurl's verbose output as `log::trace!` records with target
//!   `curlite`, when that level is enabled.
//! * `http2`, `static-curl`, `static-ssl`, `rustls`: passed through to the
//!   [`curl`] crate, see below.
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
mod client;
mod error;
mod request;
mod response;
mod transport;

pub use body::Multipart;
pub use client::Client;
pub use error::{Error, Result};
pub use request::{HttpVersion, Request, delete, get, head, patch, post, put};
pub use response::{Response, ResponsePart};
pub use transport::ResponseReader;

pub use bytes::Bytes;
pub use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
pub use mime::Mime;
