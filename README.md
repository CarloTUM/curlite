# curlite

[![CI](https://github.com/CarloTUM/curlite/actions/workflows/ci.yml/badge.svg)](https://github.com/CarloTUM/curlite/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/curlite.svg)](https://crates.io/crates/curlite)
[![docs.rs](https://docs.rs/curlite/badge.svg)](https://docs.rs/curlite)

A small blocking HTTP client on libcurl. It uses the machine's proxies,
certificate store and credentials, so a program on curlite behaves like
`curl` does on that host. For CLI tools, cron jobs, plugins, distro packages
and small devices: no async runtime, 20 crates, and the HTTP features of
whatever libcurl is linked.

```rust
use curlite::{Multipart, mime};

fn main() -> curlite::Result<()> {
    let response = curlite::post("https://example.com/upload?debug=1")
        .bearer_auth("secret")
        .multipart(
            Multipart::new()
                .text("description", "quarterly report")
                .file("file", "q3.pdf", mime::APPLICATION_PDF, std::fs::read("q3.pdf")?),
        )
        .send()?;

    println!("{}", response.status());
    // a multipart response, split into parts that borrow the response buffer
    for part in response.parts()? {
        println!("{}: {} bytes", part.name, part.body.len());
    }
    Ok(())
}
```

## What you get

- **The environment, honoured.** `http_proxy`, `no_proxy` and friends, SOCKS,
  the system certificate store, `~/.netrc`, and Kerberos tickets or NTLM where
  the linked libcurl supports them (the distro one does, a `static-curl`
  build only with the `curl` crate's `spnego` and `ntlm` features).
- **HTTP/2, and HTTP/3** where libcurl has it.
- **Bodies both ways, held once.** Raw bytes, forms, JSON, a file streamed
  from disk, `multipart/form-data` sent part by part; multipart responses
  split into slices of the response buffer, nested ones too.
- **Streaming with backpressure.** `send_to(&mut writer)`, or `send_reader()`
  for an `impl Read` that pulls from the socket on demand, and a progress
  callback for both directions.
- **Timeouts that fit downloads.** No total limit by default; a transfer fails
  when nothing moves for 30 seconds.
- **`to_curl()`** prints any request as a `curl` command line, credentials as
  shell variables, for logs and bug reports.

Against the other blocking clients, from their sources on 2026-10-11
(ureq 3.4.2, reqwest 0.12.28 `blocking`):

| | curlite | ureq | reqwest |
|---|---|---|---|
| Pure Rust, no C toolchain | no | yes | yes |
| Async API | no | no | yes, blocking is a wrapper |
| HTTP/2 | yes | no | yes |
| HTTP/3 | with a libcurl built with it (static build: [curl-rust#688](https://github.com/sagebind/curl-rust/pull/688), unreleased) | no | unstable feature |
| Proxy from the environment, SOCKS | yes | yes, feature | yes, feature |
| Basic, Bearer | yes | set the header yourself | yes |
| Digest | yes | no | no |
| Kerberos (Negotiate), NTLM | with the system libcurl | no | no |
| `~/.netrc` | yes | no | no |
| Unix domain socket | yes | custom transport | yes |
| Client certificate | yes | yes | yes |
| Multipart responses | yes | no | no |
| Progress callback | yes | no | no |
| Default timeout | 30 s without data | none | 30 s per request |
| Request as a `curl` command | yes | no | no |
| Background runtime thread | no | no | tokio |
| Crates, default features | 20 | 22 | 85 |
| TLS | libcurl's: OpenSSL, GnuTLS, Schannel, Secure Transport, or static rustls | rustls, native-tls | rustls, native-tls |

Crates are `cargo tree -e normal`, unique names, the crate itself included.
ureq is the better choice when the program must be pure Rust, reqwest when it
is async anyway. curlite is for the rest: the program that must work in a
corporate network on Monday, on a Debian box with the distro libcurl, and on a
router with 64 MiB of RAM, with one binary each.

## Requests and responses

`curlite::get(url)` (and `post`, `put`, `patch`, `delete`, `head`) starts a
request with the defaults; a `Client` holds headers, credentials, timeouts and
connection settings that several requests share, and starts requests the same
way. On the request:

| | |
|---|---|
| `query`, `header` | URL parameter, header; `headers` replaces them all |
| `body(mime, bytes)`, `form(pairs)`, `json(&value)`, `multipart(form)`, `body_reader(mime, reader)` | the body: raw, urlencoded, JSON (`json` feature), `multipart/form-data`, streamed from a `Read + Seek` |
| `basic_auth`, `bearer_auth`, `auth(Auth)`, `netrc` | credentials; `Auth` also has `Digest`, `Negotiate` and `Ntlm` |
| `stall_timeout`, `connect_timeout`, `timeout` | 30 s without data, 30 s to connect, no total limit |
| `follow_redirects`, `max_redirects` | on, 10 |
| `http_version`, `proxy`, `unix_socket` | force a version, a proxy instead of the environment's, a socket instead of the host |
| `ca_bundle`, `client_cert`, `danger_accept_invalid_certs` | TLS trust and client authentication |
| `compression`, `max_body_size`, `on_progress` | ask for compressed responses (on), bound `send` (64 MiB), byte counts |
| `send`, `send_to(&mut writer)`, `send_reader` | body into memory, into a writer, or as an `impl Read` with backpressure |
| `to_curl` | the request as a `curl` command line |

On the response: `url` (after redirects), `status`, `headers`, `body` (a
`Bytes`), `text`, `json`, `form`, `parts` (multipart, borrowing the buffer),
`error_for_status`.

```rust
let api = curlite::Client::new()
    .header(curlite::header::USER_AGENT, curlite::HeaderValue::from_static("tool/1.0"))
    .netrc();
let mut file = std::fs::File::create("big.iso")?;
api.get("https://example.com/big.iso")
    .on_progress(|p| eprintln!("{} of {:?}", p.downloaded, p.download_total))
    .send_to(&mut file)?;
```

curlite follows redirects itself, up to `max_redirects`, so that it can do
what RFC 9110 says: `303` becomes `GET`, `301` and `302` turn a `POST` into
`GET`, `307` and `308` resend the body. libcurl would repeat a `DELETE` on
every hop. Only `http` and `https` targets are followed, and `Authorization`,
`Cookie` and the request's credentials are dropped when a redirect leaves the
origin; other headers travel.

## TLS and linking

TLS comes from libcurl. By default the system libcurl is linked dynamically,
with whatever TLS library it was built against. That needs the development
package (`libcurl4-openssl-dev` on Debian and Ubuntu, `libcurl-devel` on
Fedora, `curl-dev` on Alpine, `curl` from Homebrew). When `pkg-config` finds
no libcurl, the [`curl`](https://crates.io/crates/curl) crate builds one from
source and links it statically against the system OpenSSL, without saying so.
The `static-curl`, `static-ssl`, `rustls` and `http2` features are passed
through to the `curl` crate to ask for a static build explicitly.

## Building for small processes

What a short-lived process pays before its first byte is decided by the
build, not by this crate. One `GET` per process on an Orange Pi Zero 2W
(Cortex-A53), CPU time and peak RSS, 20 runs each; the full table, the method
and the x86 numbers are in [curlite-bench](https://github.com/CarloTUM/curlite-bench):

| build | plain HTTP | HTTPS | binary |
|---|---|---|---|
| system `libcurl4` (34 shared libraries) | 36.7 ms, 12.0 MiB | 75 ms | 0.5 MiB |
| `static-curl` + `static-ssl` (OpenSSL 3) | 10.4 ms, 5.5 MiB | 48 ms | 6.2 MiB |
| `static-curl` + `rustls` (aws-lc), `AWS_LC_SYS_NO_JITTER_ENTROPY=1` | 5.5 ms, 3.0 MiB | 21 ms | 3.4 MiB |
| static musl, rustls on ring | 3.0 ms, 1.1 MiB | 26 ms | 2.0 MiB |
| ureq 3, for comparison | 4.3 ms, 2.4 MiB | 21 ms | 1.6 MiB |

An empty C program costs 1.5 ms and 0.7 MiB on that board. The TLS library
decides, not the HTTP client: OpenSSL 3 spends about 8 ms and 4.7 MiB on its
own initialisation, which libcurl runs even for plain HTTP. aws-lc, the
default crypto provider behind `rustls`, seeds its RNG from CPU jitter, which
costs hundreds of milliseconds per process on a slow core unless
`AWS_LC_SYS_NO_JITTER_ENTROPY=1` is set at build time. The distro libcurl
loads everything it was built with, 34 shared objects on Debian.

The leanest configuration, a static musl binary with rustls on ring:

```toml
# Cargo.toml of the program
[dependencies]
curlite = { version = "0.2", default-features = false }
curl = { version = "0.4", default-features = false, features = ["rustls-ring", "static-curl"] }

# .cargo/config.toml
[target.aarch64-unknown-linux-musl]
rustflags = ["-C", "target-feature=+crt-static", "-C", "relocation-model=static"]
```

```sh
CC_aarch64_unknown_linux_musl=musl-gcc cargo build --release --target aarch64-unknown-linux-musl
```

The `rustls-ring` feature of the `curl` crate is not released yet
([curl-rust#687](https://github.com/sagebind/curl-rust/pull/687); the
benchmark repository carries the patch). Until it lands, `rustls` with
`AWS_LC_SYS_NO_JITTER_ENTROPY=1` at build time gives the same cold start and
needs cmake. HTTP/3 in a static build is
[curl-rust#688](https://github.com/sagebind/curl-rust/pull/688), passed
through here once released.

## How it runs

Each thread owns one libcurl multi handle, which holds the connection cache,
and one easy handle that is reused from request to request. A request is
driven step by step, so a `send_reader()` body is pulled from the socket on
demand and libcurl pauses the transfer when 128 KiB are buffered and not yet
read. Writers, response readers and progress callbacks run between those
steps, never inside libcurl's callbacks, so they may send requests of their
own. A request body reader is read from inside libcurl's read callback; a
request it sends from there gets a private handle. A `Client` is plain
configuration, `Send` and `Sync`; the libcurl handles belong to the thread
that sends.

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

## Stability

Minimum Rust 1.88 (edition 2024), tested in CI and raised only with a minor
release. `cargo semver-checks` compares every push against the published
API, and a [fuzz target](fuzz) runs the multipart parser, the one piece of
curlite that parses bytes off the network, on every push. TLS client
authentication is tested end to end against `openssl s_server`, on the
OpenSSL and the rustls backend.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
