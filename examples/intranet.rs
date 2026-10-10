//! Fetches a URL the way `curl --negotiate -u : --netrc-optional` would: with
//! the Kerberos ticket of the process when the server asks for it, with
//! `~/.netrc` credentials otherwise, through whatever proxy the environment
//! names. On failure it prints the request as a curl command to retry by hand.
//! Negotiate needs a libcurl built with GSS-API, which the system one is; a
//! `static-curl` build without the `spnego` feature fails with "not built-in".
//!
//!     cargo run --example intranet -- https://wiki.corp.example/page

use curlite::Auth;

fn main() -> Result<(), curlite::Error> {
    let url = std::env::args().nth(1).expect("url");
    let request = curlite::get(&url).auth(Auth::Negotiate).netrc();
    let command = request.to_curl();
    match request.send().and_then(|r| r.error_for_status()) {
        Ok(response) => {
            println!("{}", response.text().unwrap_or("<not UTF-8>"));
            Ok(())
        }
        Err(e) => {
            eprintln!("{e}\nreproduce with:\n{command}");
            Err(e)
        }
    }
}
