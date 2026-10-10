# curlite

[![CI](https://github.com/CarloTUM/curlite/actions/workflows/ci.yml/badge.svg)](https://github.com/CarloTUM/curlite/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/curlite.svg)](https://crates.io/crates/curlite)
[![docs.rs](https://docs.rs/curlite/badge.svg)](https://docs.rs/curlite)

A small blocking HTTP client on top of libcurl. For programs that call HTTP
services and want to stay small: no async runtime, one libcurl handle per
thread with connection reuse, request bodies held in memory once, and
`multipart/form-data` in both directions.

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

Bodies are `body(mime, bytes)`, `form(pairs)` or `multipart(..)`; everything
else (`query`, `header`, `timeout`, `follow_redirects`) is on the builder.
Responses are read into memory and expose `status()`, `headers()`, `body()`,
`text()`, `form()` and `parts()`.

The `params` module adds a parameter model for services that exchange named
values and files, where the body format follows from the parameters: one body
parameter is sent raw, several text parameters as a form, anything with a file
as multipart. Responses parse back into the same model. This is the interface
of the [CPEE](https://cpee.org) process engine.

## TLS and linking

TLS comes from libcurl. By default the system libcurl is linked dynamically,
with whatever TLS library it was built against. The `static-curl`,
`static-ssl` and `rustls` features are passed through to the
[`curl`](https://crates.io/crates/curl) crate to build and link libcurl
statically instead.

## Why libcurl

Most Rust HTTP clients are async and bring a runtime with them. That is the
right choice for servers, but for a small program that makes a few calls, or
for many small processes on one machine, it mostly adds binary size and setup
cost. `curlite` keeps the client itself small and leaves the heavy lifting to
libcurl, which is probably already on the machine and whose TLS library can
be chosen at build time.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
