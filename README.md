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
of the [CPEE](https://cpee.org) process engine, which `curlite` was written for.

## TLS and linking

TLS comes from libcurl. By default the system libcurl is linked dynamically,
with whatever TLS library it was built against. The `static-curl`,
`static-ssl` and `rustls` features are passed through to the
[`curl`](https://crates.io/crates/curl) crate to build and link libcurl
statically instead.

## Where the memory goes

`curlite` is the client developed for the bachelor's thesis *Where the Memory
Goes: Design and Evaluation of a Synchronous Rust HTTP Client for Process
Instances on Edge Devices* (TUM, 2026). The thesis replaced the
`reqwest`/`tokio` stack of a Rust process engine with this client and measured
both on an Orange Pi Zero 2W, for one process instance and for up to 50
concurrent instances, in six build configurations that differ in the TLS
library and the linking.

Short version: the async runtime was not where the memory went.

| | reqwest<br>(rustls, vendored) | curlite<br>(rustls, vendored) | curlite<br>(OpenSSL, system) | curlite<br>(wolfSSL, tiny libcurl) |
|---|---|---|---|---|
| Peak RSS, one instance | 9.88 MiB | 9.69 MiB | 19.40 MiB | **7.97 MiB** |
| Private memory per instance, 50 instances | **1.70 MiB** | 2.00 MiB | 5.99 MiB | 1.95 MiB |
| Shipped to the device | 10.67 MiB | 10.04 MiB | (system libraries) | smallest |
| 16 MiB download, max RSS of the process | 50.7 MiB | **20.0 MiB** | | |

- With many instances, everything except written pages amortizes across
  processes. What remains is dominated by the TLS library: OpenSSL costs 3–4 MiB
  of private memory per process more than wolfSSL or rustls. The HTTP client
  accounts for about 0.3 MiB, the Rust linking mode for another 0.3 MiB.
- For a single instance, the smallest footprint comes from a tiny libcurl
  with wolfSSL, 1.4–1.9 MiB RSS below the `reqwest` baseline. Only a
  curl-based client makes that configuration available.
- `curlite` holds a request body once and a response once. `reqwest` peaked
  at about three times the payload for a 16 MiB download.
- Per request, both clients cost the same. The curl/rustls build pays a
  one-time 0.47 s for the initialization of `aws-lc`'s entropy source.

The full measurements, including the heap profiles, are in the thesis; the
original client and the measurement setup are in the
[Bachelor-Thesis](https://github.com/CarloTUM/Bachelor-Thesis) repository.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
