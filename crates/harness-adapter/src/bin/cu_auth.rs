//! Private, one-request OAuth helper for the desktop host.
//!
//! This executable is not a public CLI. Main sends one bounded JSON line over
//! its private pipe and reads one bounded JSON response from stdout. It never
//! accepts credentials in arguments or environment variables and emits no
//! diagnostics that could expose OAuth material.

use std::io::{self, Read, Write};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use lingxi_llm_client::auth::oauth::openai::{
    ExchangedTokens, OpenAiOAuthConfig, exchange_code, parse_id_token, refresh_token,
};
use lingxi_llm_client::{HttpTransport, Transport};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

const MAX_INPUT_MESSAGE_BYTES: usize = 32 * 1024;
const MAX_OUTPUT_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
const ALLOWED_PORTS: [u16; 2] = [1455, 1457];
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const AUTH_SCOPES: &str = "openid profile email offline_access";

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Start {
        port: u16,
    },
    Exchange {
        code: Zeroizing<String>,
        verifier: Zeroizing<String>,
        redirect_uri: Zeroizing<String>,
    },
    Refresh {
        refresh_token: Zeroizing<String>,
    },
    Catalog,
    Models {
        provider_id: String,
        credential: harness_adapter::model::provider_catalog::ModelCredential,
    },
}

#[derive(Serialize)]
struct StartResponse<'a> {
    url: &'a str,
    verifier: &'a str,
    state: &'a str,
    redirect_uri: &'a str,
}

#[derive(Serialize)]
struct ExchangeResponse<'a> {
    access_token: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<&'a str>,
    expires_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fedramp: Option<bool>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

#[derive(Serialize)]
struct ModelsResponse {
    models: Vec<harness_adapter::model::provider_catalog::ProviderModel>,
}

fn config() -> OpenAiOAuthConfig {
    OpenAiOAuthConfig {
        scopes: AUTH_SCOPES.into(),
        loopback_ports: ALLOWED_PORTS,
        ..OpenAiOAuthConfig::default()
    }
}

fn read_request() -> Result<Request, ()> {
    let stdin = io::stdin();
    let mut bytes = Vec::new();
    if stdin
        .lock()
        .take((MAX_INPUT_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        bytes.zeroize();
        return Err(());
    }
    if bytes.is_empty() || bytes.len() > MAX_INPUT_MESSAGE_BYTES {
        bytes.zeroize();
        return Err(());
    }

    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.contains(&b'\n') || bytes.contains(&b'\r') {
        bytes.zeroize();
        return Err(());
    }
    let parsed = serde_json::from_slice(&bytes).map_err(|_| ());
    bytes.zeroize();
    parsed
}

fn serialize<T: Serialize>(value: &T) -> Result<Vec<u8>, ()> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| ())?;
    if bytes.len() + 1 > MAX_OUTPUT_MESSAGE_BYTES {
        bytes.zeroize();
        return Err(());
    }
    bytes.push(b'\n');
    Ok(bytes)
}

fn unix_now_seconds() -> Result<u64, ()> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| ())
}

fn valid_redirect(config: &OpenAiOAuthConfig, redirect_uri: &str) -> bool {
    ALLOWED_PORTS
        .iter()
        .any(|port| config.redirect_uri(*port) == redirect_uri)
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_TOKEN_BYTES && !token.chars().any(char::is_control)
}

// Refresh responses may omit identity and rotation. Main preserves the previous
// session fields in that case; refresh material never goes to the model runtime.
fn token_response(tokens: &mut ExchangedTokens, refreshing: bool) -> Result<Vec<u8>, ()> {
    let access_token = Zeroizing::new(std::mem::take(&mut tokens.access_token));
    let refresh = tokens.refresh_token.take().map(Zeroizing::new);
    let id_token = tokens.id_token.take().map(Zeroizing::new);
    if !valid_token(&access_token) || refresh.as_ref().is_some_and(|token| !valid_token(token)) {
        return Err(());
    }
    let claims = id_token.as_ref().and_then(|token| parse_id_token(token));
    let now = unix_now_seconds()?;
    let expires_at = now
        .checked_add(tokens.effective_lifetime().as_secs())
        .filter(|expires_at| *expires_at > now)
        .ok_or(())?;
    serialize(&ExchangeResponse {
        access_token: &access_token,
        refresh_token: refresh.as_ref().map(|token| token.as_str()),
        expires_at,
        account_id: claims
            .as_ref()
            .and_then(|claims| claims.account_id.as_deref()),
        fedramp: claims
            .as_ref()
            .map(|claims| claims.fedramp)
            .or(if refreshing { None } else { Some(false) }),
    })
}

async fn handle(request: Request) -> Result<Vec<u8>, ()> {
    match request {
        Request::Start { port } => {
            if !ALLOWED_PORTS.contains(&port) {
                return Err(());
            }
            let config = config();
            let (url, verifier, state) = config.build_authorize_url(port);
            let url = Zeroizing::new(url);
            let redirect_uri = config.redirect_uri(port);
            let verifier = Zeroizing::new(verifier);
            let state = Zeroizing::new(state);
            serialize(&StartResponse {
                url: url.as_str(),
                verifier: verifier.as_str(),
                state: state.as_str(),
                redirect_uri: &redirect_uri,
            })
        }
        Request::Exchange {
            code,
            verifier,
            redirect_uri,
        } => {
            let config = config();
            if code.is_empty()
                || code.len() > MAX_TOKEN_BYTES
                || code.chars().any(char::is_control)
                || verifier.is_empty()
                || verifier.len() > 256
                || verifier.chars().any(char::is_control)
                || !valid_redirect(&config, &redirect_uri)
            {
                return Err(());
            }

            let transport = HttpTransport::with_client_configurator(|client| client.no_proxy())
                .map_err(|_| ())?;
            let mut tokens = exchange_code(
                &transport as &dyn Transport,
                &config,
                &code,
                &verifier,
                &redirect_uri,
            )
            .await
            .map_err(|_| ())?;

            token_response(&mut tokens, false)
        }
        Request::Refresh {
            refresh_token: token,
        } => {
            if !valid_token(&token) {
                return Err(());
            }
            let transport = HttpTransport::with_client_configurator(|client| client.no_proxy())
                .map_err(|_| ())?;
            let mut tokens = refresh_token(&transport as &dyn Transport, &config(), &token)
                .await
                .map_err(|_| ())?;
            token_response(&mut tokens, true)
        }
        Request::Catalog => serialize(&harness_adapter::model::provider_catalog::catalog()),
        Request::Models {
            provider_id,
            credential,
        } => {
            let transport = HttpTransport::with_client_configurator(|client| client.no_proxy())
                .map_err(|_| ())?;
            let models = harness_adapter::model::provider_catalog::models(
                &provider_id,
                &credential,
                &transport,
            )
            .await?;
            serialize(&ModelsResponse { models })
        }
    }
}

fn run() -> Result<Vec<u8>, ()> {
    let request = read_request()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| ())?;
    runtime.block_on(handle(request))
}

fn main() -> ExitCode {
    let (mut output, outcome) = match run() {
        Ok(output) => (output, ExitCode::SUCCESS),
        Err(()) => match serialize(&ErrorResponse {
            error: "authorization unavailable",
        }) {
            Ok(output) => (output, ExitCode::FAILURE),
            Err(()) => return ExitCode::FAILURE,
        },
    };
    let status = if io::stdout().lock().write_all(&output).is_ok() {
        outcome
    } else {
        ExitCode::FAILURE
    };
    output.zeroize();
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_wire_is_strict_and_preserves_missing_identity() {
        assert!(matches!(
            serde_json::from_str::<Request>(
                r#"{"operation":"refresh","refresh_token":"test-refresh"}"#
            ),
            Ok(Request::Refresh { .. })
        ));
        assert!(serde_json::from_str::<Request>(
            r#"{"operation":"refresh","refresh_token":"test-refresh","base_url":"https://other.test"}"#).is_err());
        let mut tokens = ExchangedTokens {
            access_token: "test-access".into(),
            refresh_token: Some("test-rotated".into()),
            id_token: None,
            expires_in: 3600,
        };
        let response: serde_json::Value =
            serde_json::from_slice(&token_response(&mut tokens, true).unwrap()).unwrap();
        assert_eq!(response["refresh_token"], "test-rotated");
        assert!(response.get("fedramp").is_none());
        assert!(response.get("account_id").is_none());
        assert!(tokens.access_token.is_empty());
        assert!(tokens.refresh_token.is_none());
        tokens.access_token = "test-access".into();
        let response: serde_json::Value =
            serde_json::from_slice(&token_response(&mut tokens, false).unwrap()).unwrap();
        assert_eq!(response["fedramp"], false);
        assert!(response.get("refresh_token").is_none());
        assert!(!valid_token("bad\ntoken"));
        assert!(!valid_token(&"x".repeat(MAX_TOKEN_BYTES + 1)));
    }

    #[test]
    fn oauth_scopes_and_loopback_redirects_are_pinned() {
        let config = config();
        assert_eq!(config.scopes, "openid profile email offline_access");
        assert_eq!(config.loopback_ports, ALLOWED_PORTS);
        assert!(valid_redirect(
            &config,
            "http://localhost:1455/auth/callback"
        ));
        assert!(valid_redirect(
            &config,
            "http://localhost:1457/auth/callback"
        ));
        assert!(!valid_redirect(
            &config,
            "http://localhost:1456/auth/callback"
        ));
        assert!(!valid_redirect(
            &config,
            "https://localhost:1455/auth/callback"
        ));
    }

    #[test]
    fn start_request_contains_only_expected_callback_ports() {
        let config = config();
        for port in ALLOWED_PORTS {
            let redirect_uri = config.redirect_uri(port);
            assert!(valid_redirect(&config, &redirect_uri));
        }
        assert!(!ALLOWED_PORTS.contains(&1456));
    }

    #[test]
    fn private_catalog_operations_are_strict_and_output_has_its_own_budget() {
        assert!(matches!(
            serde_json::from_str::<Request>(r#"{"operation":"catalog"}"#),
            Ok(Request::Catalog)
        ));
        assert!(serde_json::from_str::<Request>(
            r#"{"operation":"models","provider_id":"gemini","credential":{"kind":"api_key","api_key":"key","bearer":true}}"#
        )
        .is_err());

        let output = serialize(&"x".repeat(64 * 1024)).unwrap();
        assert!(output.len() > MAX_INPUT_MESSAGE_BYTES);
        assert!(output.len() < MAX_OUTPUT_MESSAGE_BYTES);
        assert!(serialize(&"x".repeat(MAX_OUTPUT_MESSAGE_BYTES)).is_err());
    }
}
