# curlite

[![CI](https://github.com/CarloTUM/curlite/actions/workflows/ci.yml/badge.svg)](https://github.com/CarloTUM/curlite/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/curlite.svg)](https://crates.io/crates/curlite)
[![docs.rs](https://docs.rs/curlite/badge.svg)](https://docs.rs/curlite)

A small blocking HTTP client on top of libcurl. For programs that call HTTP
services and want to stay small: no async runtime, one libcurl handle per
thread with connection reuse, request bodies held in memory once (or streamed
from a file), response bodies in memory once (or streamed to a writer), and
`multipart/form-data` in both directions without copying the parts.

```rust
use curlite::Multipart;

let response = curlite::post("https://example.com/upload?debug=1")
    .header("Authorization", "Bearer secret")
    .multipart(
        Multipart::new()
            .text("description", "quarterly report")
            .file("file", "q3.pdf", mime::APPLICATION_PDF, std::fs::read("q3.pdf")?),
    )
    .send()?;

println!("{}", response.status());
for part in response.parts()? {
    println!("{}: {} bytes", part.name, part.body.len());
}
```

Bodies are `body(mime, bytes)`, `body_reader(mime, file)`, `form(pairs)` or
`multipart(..)`; everything else (`query`, `header`, `timeout`,
`connect_timeout`, `follow_redirects`) is on the builder. `send()` reads the
response into memory, `send_to(&mut writer)` streams it. Responses expose
`status()`, `headers()`, `body()`, `text()`, `form()` and `parts()`, where
multipart parts are slices of the response buffer. A `Client` holds headers
and timeouts shared by several requests.

Redirects are followed by `curlite` itself, with the verb handling of RFC
9110: `303` turns any method into `GET`, `301`/`302` do so for `POST`,
`307`/`308` resend the body. `Authorization` is dropped when a redirect leaves
the origin. libcurl's own following is not used because it keeps a custom verb
such as `DELETE` across every redirect.

The `params` module adds a parameter model for services that exchange named
values and files, where the body format follows from the parameters: one body
parameter is sent raw, several text parameters as a form, anything with a file
as multipart. Responses parse back into the same model. This is the interface
of the [CPEE](https://cpee.org) process engine.

## TLS and linking

TLS comes from libcurl. By default the system libcurl is linked dynamically,
with whatever TLS library it was built against. That needs the development
package (`libcurl4-openssl-dev` on Debian and Ubuntu, `libcurl-devel` on
Fedora, `curl-dev` on Alpine, `curl` from Homebrew); without it, the
[`curl`](https://crates.io/crates/curl) crate silently builds libcurl from
source and links it statically against the system OpenSSL. The `static-curl`,
`static-ssl` and `rustls` features are passed through to the `curl` crate to
ask for a static build explicitly.

## Why libcurl

Most Rust HTTP clients are async and bring a runtime with them. That is the
right choice for servers, but for a small program that makes a few calls, or
for many small processes on one machine, it mostly adds binary size and setup
cost. `curlite` keeps the client itself small and leaves the heavy lifting to
libcurl, which is probably already on the machine and whose TLS library can
be chosen at build time.

Small also means few dependencies: besides `curl`, `curlite` uses `bytes`,
`http`, `mime`, `form_urlencoded` and `thiserror`. URL parsing is libcurl's,
multipart encoding and parsing are a few hundred lines in this crate.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
