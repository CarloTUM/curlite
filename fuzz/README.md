Fuzz targets for the parsers in curlite, run with
[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) on a nightly toolchain:

```sh
cargo +nightly fuzz run multipart
```

`multipart` feeds the `multipart/*` splitter (`Response::parts`,
`ResponsePart::parts`) arbitrary boundaries and bodies, nested four levels
deep. It is the only code in curlite that parses bytes from the network
beyond what libcurl hands over as header lines.
