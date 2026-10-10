# Security

Report a vulnerability privately through GitHub's "Report a vulnerability"
button on this repository (Security tab), or by e-mail to the address in the
commit log. You will get an answer within a few days, and a fix or a
workaround before anything is published.

What this crate does with untrusted bytes, so you know where to look:

- Response headers and `multipart/*` bodies are parsed by this crate
  (`src/response.rs`). The multipart parser is fuzzed on every push
  (`fuzz/`).
- Everything else that touches the network is libcurl's. A libcurl
  vulnerability applies to curlite as to curl; keep the system libcurl
  updated, or rebuild with a newer `curl-sys` for static builds.
- Redirects are followed by this crate: only to `http` and `https`, and
  `Authorization`, `Cookie` and request credentials are dropped when the
  target is another origin. Other headers and the proxy, Unix socket and
  client certificate settings apply to every hop, as in curl.
- `send()` holds at most `max_body_size` (64 MiB by default) in memory.
- `to_curl()` prints credentials as shell variables, never as values; other
  headers and the URL's query string are printed as they are.
