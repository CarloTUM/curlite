# curlite

[![CI](https://github.com/CarloTUM/curlite/actions/workflows/ci.yml/badge.svg)](https://github.com/CarloTUM/curlite/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/curlite.svg)](https://crates.io/crates/curlite)
[![docs.rs](https://docs.rs/curlite/badge.svg)](https://docs.rs/curlite)

A small blocking HTTP client on top of libcurl, for short-lived processes:
CLI tools, hooks, cron jobs, plugins and small devices, where an async
runtime per invocation is pure overhead. No runtime, one connection cache
per thread, request bodies held in memory once (or streamed from a file),
response bodies in memory once (or streamed), `multipart/form-data` in both
directions without copying the parts, and everything libcurl already does:
HTTP/2, proxies from the environment, Unix sockets, the system certificate
store, transparent decompression.

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

Bodies are `body(mime, bytes)`, `body_reader(mime, file)`, `form(pairs)`,
`multipart(..)` or, with the `json` feature, `json(&value)`; everything else
(`query`, `header`, `timeout`, `connect_timeout`, `follow_redirects`,
`http_version`, `proxy`, `unix_socket`, `ca_bundle`, `compression`) is on
the builder. `send()` reads the response into memory, `send_to(&mut writer)`
streams it, `send_reader()` returns an `impl Read` that pulls from the
connection with backpressure. Responses expose `status()`, `headers()`,
`body()`, `text()`, `json()`, `form()`, `parts()` (multipart parts as slices
of the response buffer) and `error_for_status()`. A `Client` holds headers,
timeouts and connection settings shared by several requests.

```rust
let api = curlite::Client::new()
    .header(curlite::header::USER_AGENT, curlite::HeaderValue::from_static("tool/1.0"))
    .timeout(std::time::Duration::from_secs(10));
let mut file = std::fs::File::create("big.iso")?;
api.get("https://example.com/big.iso").send_to(&mut file)?;
```

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

## How it runs

Each thread owns one libcurl multi handle, which holds the connection cache,
and one easy handle that is reused from request to request. A request is
driven step by step, so a `send_reader()` body is pulled from the socket on
demand and libcurl pauses the transfer when 128 KiB are buffered and not yet
read. Writers and readers run between those steps, never inside libcurl's
callbacks, so they may send requests of their own.

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
