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
(`query`, `header`, `basic_auth`, `bearer_auth`, `auth`, `netrc`,
`stall_timeout`, `timeout`, `connect_timeout`, `follow_redirects`,
`http_version`, `proxy`, `unix_socket`, `ca_bundle`, `client_cert`,
`compression`, `on_progress`) is on the builder. `send()` reads the response into memory,
`send_to(&mut writer)` streams it, `send_reader()` returns an `impl Read`
that pulls from the connection with backpressure. Responses expose `url()`,
`status()`, `headers()`, `body()`, `text()`, `json()`, `form()`, `parts()`
(multipart parts as slices of the response buffer) and `error_for_status()`.
A `Client` holds headers, timeouts and connection settings shared by several
requests, and `to_curl()` prints any request as a `curl` command line for
logs and bug reports.

Authentication is libcurl's: `Auth::Basic`, `Bearer`, `Digest`, `Negotiate`
(Kerberos, with the ticket of the process) and `Ntlm`, and `netrc()` takes
the credentials for a host from `~/.netrc`. Credentials, like an
`Authorization` header, are dropped when a redirect leaves the origin.

There is no total time limit by default. A transfer fails when nothing has
moved for 30 seconds (`stall_timeout`) or the connection takes longer than 30
seconds to establish (`connect_timeout`); a large download that keeps
flowing is never cut off. `timeout` adds a hard limit per transfer.

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

## TLS and linking

TLS comes from libcurl. By default the system libcurl is linked dynamically,
with whatever TLS library it was built against. That needs the development
package (`libcurl4-openssl-dev` on Debian and Ubuntu, `libcurl-devel` on
Fedora, `curl-dev` on Alpine, `curl` from Homebrew); without it, the
[`curl`](https://crates.io/crates/curl) crate silently builds libcurl from
source and links it statically against the system OpenSSL. The `static-curl`,
`static-ssl` and `rustls` features are passed through to the `curl` crate to
ask for a static build explicitly.

## Building for small processes

What a short-lived process pays before its first byte is decided by the
build, not by this crate. Measured on an Orange Pi Zero 2W (Cortex-A53,
Debian 13) for one `GET` per process, CPU time and peak RSS:

| libcurl | TLS | plain HTTP | HTTPS | binary |
|---|---|---|---|---|
| system (`libcurl4`, 34 shared libraries) | OpenSSL 3, dynamic | 36.7 ms, 12.0 MiB | 75 ms | 0.5 MiB |
| `static-curl` | OpenSSL 3, dynamic | 17.0 ms, 6.9 MiB | 55 ms | 1.1 MiB |
| `static-curl` + `static-ssl` | OpenSSL 3, static | 10.4 ms, 5.5 MiB | 48 ms | 6.2 MiB |
| `static-curl` + `rustls` | aws-lc, jitter entropy on | 5.9 ms, 3.0 MiB | **433 ms** | 3.4 MiB |
| `static-curl` + `rustls`, `AWS_LC_SYS_NO_JITTER_ENTROPY=1` | aws-lc | 5.5 ms, 3.0 MiB | 21 ms | 3.4 MiB |
| `static-curl` + rustls on ring, non-PIE | ring | 4.7 ms, 2.6 MiB | 28 ms | 1.9 MiB |
| same, static musl binary | ring | **3.0 ms, 1.1 MiB** | 26 ms | 2.0 MiB |
| static musl, `rustls`, no jitter entropy | aws-lc | 3.3 ms, 1.4 MiB | **19 ms** | 3.3 MiB |
| ureq 3 (rustls), for comparison | rustls | 4.3 ms, 2.4 MiB | 21 ms | 1.6 MiB |
| minreq, for comparison | rustls | 3.5 ms, 2.1 MiB | 20 ms | 1.2 MiB |

An empty C program costs 1.5 ms and 0.7 MiB on that board. The lessons:

- **The TLS library is the cost, not the HTTP client.** OpenSSL 3 spends
  about 8 ms of CPU and 4.7 MiB on its own initialisation, which libcurl
  runs even for plain HTTP. With rustls that work disappears.
- **aws-lc's CPU jitter entropy** (rustls-ffi's default crypto provider)
  burns hundreds of milliseconds per process on a slow core seeding its
  RNG. Build with `AWS_LC_SYS_NO_JITTER_ENTROPY=1`, or use ring.
- **The distro libcurl drags in everything** it was built with: on Debian
  that is GnuTLS via librtmp, Kerberos, LDAP, libssh2 and more, 34 shared
  objects and 57 000 relocations per process start. `static-curl` builds a
  libcurl that knows HTTP(S) and nothing else.
- `-C relocation-model=static` (no PIE) removes the executable's own
  relocations; a static musl build removes the dynamic loader entirely.

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

With `rustls` instead of `rustls-ring` (and `AWS_LC_SYS_NO_JITTER_ENTROPY=1`
in the build environment) the same recipe gives the fastest HTTPS on the
board, at the price of cmake at build time and 1.3 MiB more binary.

The `rustls-ring` feature of the `curl` crate is not upstream yet; the
[benchmark repository](https://github.com/CarloTUM/curlite-bench) carries
the patch, the build recipe and the measurement scripts. Until it lands,
`rustls` with `AWS_LC_SYS_NO_JITTER_ENTROPY=1` in the environment of the
build gets the same cold start and a slightly faster handshake on ARM.

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
