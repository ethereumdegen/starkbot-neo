//! The loopback leg of an authorization-code login.
//!
//! A one-shot HTTP listener on `127.0.0.1:<callback_port>` answers exactly one
//! successful request to the provider's callback path and shuts down. There is
//! no web framework here on purpose: the whole protocol surface is one request
//! line, and a dependency on a server stack would be a much larger thing to
//! audit than `TcpListener` plus a `split_whitespace`.
//!
//! The paste-the-URL fallback and the loopback path share
//! [`validate_redirect`], so a state mismatch is caught identically whichever
//! way the code arrives.

use std::fmt;
use std::net::TcpListener as StdTcpListener;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use url::Url;
use zeroize::Zeroize;

use super::OauthError;

/// Nothing legitimate sends a request line longer than this; a browser
/// redirect with a code and a state is a few hundred bytes.
const MAX_REQUEST_LINE: usize = 8 * 1024;

/// One authorization code, with the `state` it arrived under.
///
/// Single-use login material: redacted formatting, wiped on drop, and the
/// accessors are crate-internal so it can only leave through a token request.
pub struct AuthCode {
    code: String,
    state: String,
}

impl AuthCode {
    pub(crate) fn new(code: String, state: String) -> Self {
        Self { code, state }
    }

    pub(crate) fn code(&self) -> &str {
        &self.code
    }

    /// The state the vendor echoed back. Anthropic's token endpoint wants it
    /// repeated in the exchange body.
    pub(crate) fn state(&self) -> &str {
        &self.state
    }
}

impl Drop for AuthCode {
    fn drop(&mut self) {
        self.code.zeroize();
    }
}

impl fmt::Debug for AuthCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthCode(••••)")
    }
}

impl fmt::Display for AuthCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, formatter)
    }
}

/// Bind the loopback port before the browser is opened, so a port already
/// held by another CLI is reported while the user is still looking at us.
pub(crate) fn bind(port: u16) -> Result<StdTcpListener, OauthError> {
    let listener = StdTcpListener::bind(("127.0.0.1", port)).map_err(|error| {
        OauthError::Listener {
            port,
            detail: error.to_string(),
        }
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|error| OauthError::Listener {
            port,
            detail: error.to_string(),
        })?;
    Ok(listener)
}

/// The port a bound listener actually got, which matters when a provider asks
/// for an ephemeral port.
pub(crate) fn bound_port(listener: &StdTcpListener) -> u16 {
    listener.local_addr().map_or(0, |addr| addr.port())
}

/// Serve the callback until the code arrives or `timeout` elapses.
pub(crate) async fn wait_for_code(
    listener: StdTcpListener,
    path: &str,
    expected_state: &str,
    timeout: Duration,
) -> Result<AuthCode, OauthError> {
    let port = bound_port(&listener);
    let listener = TcpListener::from_std(listener).map_err(|error| OauthError::Listener {
        port,
        detail: error.to_string(),
    })?;
    let serve = async {
        loop {
            let (stream, _) = listener
                .accept()
                .await
                .map_err(|error| OauthError::Listener {
                    port,
                    detail: error.to_string(),
                })?;
            // A browser also asks for `/favicon.ico` and may pre-connect;
            // only the callback path ends the loop.
            if let Some(result) = serve_one(stream, path, expected_state).await {
                return result;
            }
        }
    };
    match tokio::time::timeout(timeout, serve).await {
        Ok(result) => result,
        Err(_) => Err(OauthError::Timeout {
            seconds: timeout.as_secs(),
        }),
    }
}

/// `None` means "that request was not the callback, keep listening".
async fn serve_one(
    mut stream: TcpStream,
    path: &str,
    expected_state: &str,
) -> Option<Result<AuthCode, OauthError>> {
    let target = match read_request_target(&mut stream).await {
        Some(target) => target,
        None => {
            respond(&mut stream, "400 Bad Request", BAD_REQUEST_PAGE).await;
            return None;
        }
    };
    // The request target is origin-form (`/callback?code=…`); a base is only
    // needed to turn it into something with query parsing.
    let base = match Url::parse("http://127.0.0.1/") {
        Ok(base) => base,
        Err(error) => {
            return Some(Err(OauthError::Url {
                detail: error.to_string(),
            }));
        }
    };
    let Ok(url) = base.join(&target) else {
        respond(&mut stream, "400 Bad Request", BAD_REQUEST_PAGE).await;
        return None;
    };
    if url.path() != path {
        respond(&mut stream, "404 Not Found", NOT_FOUND_PAGE).await;
        return None;
    }
    let result = validate_redirect(&url, expected_state);
    match &result {
        Ok(_) => respond(&mut stream, "200 OK", SUCCESS_PAGE).await,
        Err(_) => respond(&mut stream, "400 Bad Request", FAILURE_PAGE).await,
    }
    Some(result)
}

/// Read the request line, ignoring the headers and body entirely.
async fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::with_capacity(512);
    let mut chunk = [0_u8; 512];
    let line_end = loop {
        if let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
            break end;
        }
        if buffer.len() >= MAX_REQUEST_LINE {
            return None;
        }
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    };
    buffer.truncate(line_end);
    let line = String::from_utf8_lossy(&buffer);
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    if !method.eq_ignore_ascii_case("GET") {
        return None;
    }
    Some(target.to_owned())
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // A browser that hung up before we answered is not a login failure.
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

/// The one place a redirect becomes an [`AuthCode`], used by both the loopback
/// listener and the pasted-URL fallback.
pub(crate) fn validate_redirect(url: &Url, expected_state: &str) -> Result<AuthCode, OauthError> {
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    finish(code, state, error, expected_state)
}

/// Validate a code that arrived without a URL around it.
///
/// Anthropic's consent screen hands the user `<code>#<state>` to paste; some
/// users paste only the code, in which case there is no state to compare and
/// the flow rests on the PKCE verifier alone.
pub(crate) fn validate_pasted(pasted: &str, expected_state: &str) -> Result<AuthCode, OauthError> {
    let pasted = pasted.trim();
    match pasted.split_once('#') {
        Some((code, state)) => finish(
            non_empty(code),
            non_empty(state),
            None,
            expected_state,
        ),
        None => finish(
            non_empty(pasted),
            Some(expected_state.to_owned()),
            None,
            expected_state,
        ),
    }
}

fn finish(
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    expected_state: &str,
) -> Result<AuthCode, OauthError> {
    if let Some(error) = error {
        // The vendor's short error *code* only — `access_denied`,
        // `invalid_scope`. Descriptions and bodies stay out of errors.
        return Err(OauthError::Denied { code: error });
    }
    let Some(code) = code else {
        return Err(OauthError::MissingCode);
    };
    let Some(state) = state else {
        return Err(OauthError::StateMismatch);
    };
    if state != expected_state {
        return Err(OauthError::StateMismatch);
    }
    Ok(AuthCode::new(code, state))
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

const SUCCESS_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>Starkbot Neo</title><body style=\"font:16px -apple-system,sans-serif;padding:3rem\"><h1>Signed in</h1><p>You can close this tab and go back to Starkbot Neo.</p></body>";
const FAILURE_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>Starkbot Neo</title><body style=\"font:16px -apple-system,sans-serif;padding:3rem\"><h1>Sign-in failed</h1><p>Starkbot Neo rejected this callback. Close this tab and start the login again.</p></body>";
const NOT_FOUND_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>Starkbot Neo</title><body>Not the sign-in callback.</body>";
const BAD_REQUEST_PAGE: &str =
    "<!doctype html><meta charset=\"utf-8\"><title>Starkbot Neo</title><body>Unreadable request.</body>";

#[cfg(test)]
mod tests {
    use super::*;

    fn redirect(query: &str) -> Url {
        #[allow(clippy::expect_used)]
        Url::parse(&format!("http://localhost:54545/callback?{query}"))
            .expect("test redirect is a URL")
    }

    #[test]
    fn accepts_a_matching_state() {
        #[allow(clippy::expect_used)]
        let code = validate_redirect(&redirect("code=ac_1&state=abc"), "abc").expect("valid");
        assert_eq!(code.code(), "ac_1");
        assert_eq!(code.state(), "abc");
    }

    #[test]
    fn rejects_a_mismatched_state() {
        let error = validate_redirect(&redirect("code=ac_1&state=evil"), "abc");
        assert!(matches!(error, Err(OauthError::StateMismatch)));
    }

    #[test]
    fn rejects_a_missing_state() {
        let error = validate_redirect(&redirect("code=ac_1"), "abc");
        assert!(matches!(error, Err(OauthError::StateMismatch)));
    }

    #[test]
    fn reports_a_denied_login() {
        let error = validate_redirect(
            &redirect("error=access_denied&error_description=User+said+no"),
            "abc",
        );
        match error {
            Err(OauthError::Denied { code }) => assert_eq!(code, "access_denied"),
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    #[test]
    fn reports_a_callback_with_neither_code_nor_error() {
        assert!(matches!(
            validate_redirect(&redirect("state=abc"), "abc"),
            Err(OauthError::MissingCode)
        ));
    }

    #[test]
    fn pasted_code_and_state_are_validated_like_a_redirect() {
        #[allow(clippy::expect_used)]
        let code = validate_pasted("ac_1#abc", "abc").expect("valid");
        assert_eq!(code.code(), "ac_1");
        assert!(matches!(
            validate_pasted("ac_1#evil", "abc"),
            Err(OauthError::StateMismatch)
        ));
    }

    #[test]
    fn a_bare_pasted_code_is_accepted_without_a_state() {
        #[allow(clippy::expect_used)]
        let code = validate_pasted("  ac_1  ", "abc").expect("valid");
        assert_eq!(code.code(), "ac_1");
        assert_eq!(code.state(), "abc");
        assert!(matches!(
            validate_pasted("   ", "abc"),
            Err(OauthError::MissingCode)
        ));
    }

    #[test]
    fn formatting_never_shows_the_code() {
        let code = AuthCode::new("ac_secret".to_owned(), "abc".to_owned());
        let rendered = format!("{code:?} {code}");
        assert!(!rendered.contains("ac_secret"));
    }
}
