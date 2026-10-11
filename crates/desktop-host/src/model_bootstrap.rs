//! A separate, bounded prefix on main's private host stdin pipe. This payload
//! is never a desktop Message, tracing event, Configure setting or ledger row.

use std::io::Read;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use desktop_ipc::SharedClock;
use harness_adapter::computer::OpenAiComputerTransport;
use harness_adapter::computer_worker::ComputerWorker;
use harness_adapter::model::{
    ModelPlanner, ModelProtocol, ModelProvider, ModelRoute, ModelTransport,
};
use harness_adapter::teacher::TeacherAdapter;
use serde::Deserialize;
use zeroize::Zeroizing;

const MAX_BOOTSTRAP_BYTES: usize = 32 * 1024;

pub struct ModelBootstrap {
    route: ModelRoute,
    credential: BootstrapCredential,
    purposes: Vec<ModelPurpose>,
    account_scope: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelPurpose {
    S2Planning,
    S1Teacher,
    ComputerProposals,
}

#[derive(Default)]
pub struct HostModelServices {
    pub planner: Option<ModelPlanner>,
    pub teacher: Option<TeacherAdapter<ModelTransport>>,
    pub computer: Option<ComputerWorker>,
    pub teacher_identity: Option<ModelIdentity>,
    pub purposes: Vec<ModelPurpose>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelIdentity {
    pub provider: String,
    pub model: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    purposes: Vec<ModelPurpose>,
    route: ModelRoute,
    credential: BootstrapCredential,
    #[serde(default)]
    account_scope: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum BootstrapCredential {
    #[serde(rename = "api_key")]
    ApiKey { api_key: Zeroizing<String> },
    #[serde(rename = "openai_oauth")]
    OpenAiOAuth {
        access_token: Zeroizing<String>,
        expires_at: u64,
        #[serde(default)]
        account_id: Option<String>,
        fedramp: bool,
    },
}

impl ModelBootstrap {
    pub fn read(reader: &mut impl Read) -> Result<Self, String> {
        let invalid = || "invalid private model authorization".to_string();
        let mut length = [0; 4];
        reader.read_exact(&mut length).map_err(|_| invalid())?;
        let length = u32::from_be_bytes(length) as usize;
        if length == 0 || length > MAX_BOOTSTRAP_BYTES {
            return Err(invalid());
        }
        let mut bytes = Zeroizing::new(vec![0; length]);
        reader.read_exact(&mut bytes).map_err(|_| invalid())?;
        let payload: Payload = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        let Payload {
            purposes,
            route,
            credential,
            account_scope,
        } = payload;
        route.validate().map_err(|_| invalid())?;
        if purposes.is_empty()
            || purposes
                .iter()
                .enumerate()
                .any(|(index, purpose)| purposes[..index].contains(purpose))
        {
            return Err(invalid());
        }
        let has_computer_purpose = purposes.contains(&ModelPurpose::ComputerProposals);
        match (has_computer_purpose, account_scope.as_deref()) {
            (true, Some(scope))
                if valid_account_scope(scope)
                    && supports_computer_route(&route)
                    && matches!(&credential, BootstrapCredential::ApiKey { .. }) => {}
            (false, None) => {}
            _ => return Err(invalid()),
        }
        match &credential {
            BootstrapCredential::ApiKey { api_key }
                if !supports_openai_oauth_route(&route)
                    && !api_key.is_empty()
                    && api_key.len() <= 16 * 1024
                    && !api_key.chars().any(char::is_control) => {}
            BootstrapCredential::OpenAiOAuth {
                access_token,
                expires_at,
                account_id,
                ..
            } if supports_openai_oauth_route(&route)
                && !has_computer_purpose
                && !access_token.is_empty()
                && access_token.len() <= 16 * 1024
                && !access_token.chars().any(char::is_control)
                && *expires_at > unix_now_seconds()
                && *expires_at <= 9_007_199_254_740_991
                && account_id.as_ref().is_none_or(|account| {
                    !account.is_empty()
                        && account.len() <= 512
                        && !account.chars().any(char::is_control)
                }) => {}
            _ => return Err(invalid()),
        }
        Ok(Self {
            route,
            credential,
            purposes,
            account_scope,
        })
    }

    pub fn authorizes(&self, purpose: ModelPurpose) -> bool {
        self.purposes.contains(&purpose)
    }

    pub fn purposes(&self) -> &[ModelPurpose] {
        &self.purposes
    }

    pub fn into_services(mut self, clock: SharedClock) -> Result<HostModelServices, String> {
        let teacher_identity = self
            .authorizes(ModelPurpose::S1Teacher)
            .then(|| ModelIdentity {
                provider: provider_identity(&self.route.provider_id),
                model: self.route.model.clone(),
            });
        let planner = if self.authorizes(ModelPurpose::S2Planning) {
            let transport = match &self.credential {
                BootstrapCredential::ApiKey { api_key } => ModelTransport::new(
                    self.route.clone(),
                    api_key.to_string(),
                    Arc::new(move || clock.now_ms()),
                ),
                BootstrapCredential::OpenAiOAuth {
                    access_token,
                    expires_at,
                    account_id,
                    fedramp,
                } => ModelTransport::new_chatgpt_oauth(
                    self.route.clone(),
                    access_token.to_string(),
                    *expires_at,
                    account_id.clone(),
                    *fedramp,
                    Arc::new(move || clock.now_ms()),
                ),
            }
            .map(ModelPlanner::new)
            .map_err(|e| e.to_string())?;
            Some(transport)
        } else {
            None
        };
        let teacher = if self.authorizes(ModelPurpose::S1Teacher) {
            let teacher = match &self.credential {
                BootstrapCredential::ApiKey { api_key } => TeacherAdapter::with_sdk(
                    self.route.clone(),
                    api_key.to_string(),
                    Arc::new(move || clock.now_ms()),
                ),
                BootstrapCredential::OpenAiOAuth {
                    access_token,
                    expires_at,
                    account_id,
                    fedramp,
                } => ModelTransport::new_chatgpt_oauth_teacher(
                    self.route.clone(),
                    access_token.to_string(),
                    *expires_at,
                    account_id.clone(),
                    *fedramp,
                    Arc::new(move || clock.now_ms()),
                )
                .map(|transport| {
                    TeacherAdapter::new(
                        transport,
                        provider_identity(&self.route.provider_id),
                        self.route.model.clone(),
                    )
                }),
            }
            .map_err(|e| e.to_string())?;
            Some(teacher)
        } else {
            None
        };
        let computer = if self.authorizes(ModelPurpose::ComputerProposals) {
            let account_scope = self
                .account_scope
                .take()
                .ok_or_else(|| "invalid private model authorization".to_string())?;
            let transport = OpenAiComputerTransport::new(
                self.route.clone(),
                match &self.credential {
                    BootstrapCredential::ApiKey { api_key } => api_key.to_string(),
                    BootstrapCredential::OpenAiOAuth { .. } => {
                        return Err("invalid private model authorization".into());
                    }
                },
                account_scope,
                Arc::new(move || clock.now_ms()),
            )
            .map_err(|error| error.to_string())?;
            Some(ComputerWorker::new(transport).map_err(|error| error.to_string())?)
        } else {
            None
        };
        Ok(HostModelServices {
            planner,
            teacher,
            computer,
            teacher_identity,
            purposes: self.purposes,
        })
    }
}

fn valid_account_scope(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23]
            .into_iter()
            .all(|index| bytes[index] == b'-')
        && bytes[14] == b'4'
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
        && bytes.iter().enumerate().all(|(index, byte)| {
            [8, 13, 18, 23].contains(&index)
                || byte.is_ascii_digit()
                || (b'a'..=b'f').contains(byte)
        })
}

fn supports_computer_route(route: &ModelRoute) -> bool {
    matches!(&route.provider_id, ModelProvider::OpenAi)
        && route.protocol == ModelProtocol::OpenAiResponses
        && route.base_url == "https://api.openai.com/v1"
}

fn supports_openai_oauth_route(route: &ModelRoute) -> bool {
    matches!(&route.provider_id,
        ModelProvider::OpenAiCompatible { name } if name == "openai-chatgpt")
        && route.protocol == ModelProtocol::OpenAiResponses
        && route.base_url == "https://chatgpt.com/backend-api/codex"
        && route.max_output_tokens == 2048
        && route.validate().is_ok()
}

fn unix_now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn provider_identity(provider: &ModelProvider) -> String {
    match provider {
        ModelProvider::AnthropicFirstParty => "anthropic_first_party".into(),
        ModelProvider::OpenAi => "open_ai".into(),
        ModelProvider::Gemini => "gemini".into(),
        ModelProvider::OpenAiCompatible { name } => format!("open_ai_compatible:{name}"),
        ModelProvider::Custom { name } => format!("custom:{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ACCOUNT_SCOPE: &str = "6ba7b810-9dad-4e7b-8a2a-123456789abc";

    fn frame(data: &[u8]) -> Vec<u8> {
        let mut result = (data.len() as u32).to_be_bytes().to_vec();
        result.extend(data);
        result
    }

    fn api_key(value: &str) -> serde_json::Value {
        serde_json::json!({"kind":"api_key", "api_key":value})
    }

    fn openai_oauth(expires_at: u64) -> serde_json::Value {
        serde_json::json!({
            "kind":"openai_oauth", "access_token":"test-access-token",
            "expires_at":expires_at, "fedramp":false
        })
    }

    fn openai_oauth_route() -> serde_json::Value {
        serde_json::json!({
            "provider_id":{"open_ai_compatible":{"name":"openai-chatgpt"}},
            "protocol":"open_ai_responses",
            "base_url":"https://chatgpt.com/backend-api/codex",
            "model":"gpt-6-luna", "max_output_tokens":2048
        })
    }

    #[test]
    fn private_prefix_is_bounded_and_errors_do_not_echo_credentials() {
        let secret = b"{\"api_key\":\"private-test-secret\",\"route\":{}}";
        let error = ModelBootstrap::read(&mut &frame(secret)[..]).err().unwrap();
        assert!(!error.contains("private-test-secret"));
        assert!(ModelBootstrap::read(&mut &[0, 1, 0, 0][..]).is_err());
        assert!(ModelBootstrap::read(&mut &[0, 0, 0, 2, b'{'][..]).is_err());
    }
    #[test]
    fn the_next_protocol_frame_stays_in_the_private_pipe() {
        let payload = serde_json::json!({"purposes":["s2_planning"], "credential":api_key("explicit-test-key"), "route":{
            "provider_id":"open_ai", "protocol":"open_ai_chat", "base_url":"https://example.invalid/v1",
            "model":"test-model", "max_output_tokens":4096}});
        let mut bytes = frame(&serde_json::to_vec(&payload).unwrap());
        bytes.extend(b"NEXT");
        let mut input = &bytes[..];
        assert!(ModelBootstrap::read(&mut input).is_ok());
        assert_eq!(input, b"NEXT");
    }

    #[test]
    fn purposes_are_required_unique_and_strict() {
        let route = serde_json::json!({
            "provider_id":"open_ai", "protocol":"open_ai_chat", "base_url":"https://example.invalid/v1",
            "model":"test-model", "max_output_tokens":4096
        });
        for purposes in [
            serde_json::json!([]),
            serde_json::json!(["s1_teacher", "s1_teacher"]),
            serde_json::json!(["s3_magic"]),
            serde_json::json!(["computer_proposals"]),
        ] {
            let payload =
                serde_json::json!({"purposes":purposes,"credential":api_key("key"),"route":route});
            assert!(
                ModelBootstrap::read(&mut &frame(&serde_json::to_vec(&payload).unwrap())[..])
                    .is_err()
            );
        }
        let missing = serde_json::json!({"api_key":"old-format-key","route":route});
        assert!(
            ModelBootstrap::read(&mut &frame(&serde_json::to_vec(&missing).unwrap())[..]).is_err()
        );
    }

    #[test]
    fn chatgpt_oauth_requires_the_trusted_responses_route_and_live_typed_credential() {
        let route = openai_oauth_route();
        let valid = serde_json::json!({
            "purposes":["s2_planning", "s1_teacher"],
            "credential":openai_oauth(unix_now_seconds() + 3600),
            "route":route.clone()
        });
        let bootstrap = ModelBootstrap::read(&mut &frame(&serde_json::to_vec(&valid).unwrap())[..])
            .expect("valid host-owned OAuth route");
        assert!(bootstrap.authorizes(ModelPurpose::S2Planning));
        assert!(bootstrap.authorizes(ModelPurpose::S1Teacher));
        assert!(!bootstrap.authorizes(ModelPurpose::ComputerProposals));

        let services = bootstrap
            .into_services(SharedClock::starting_now())
            .unwrap();
        assert!(services.planner.is_some());
        assert!(services.teacher.is_some());
        assert!(services.computer.is_none());

        let mut wrong_endpoint = route.clone();
        wrong_endpoint["base_url"] = serde_json::json!("https://api.openai.com/v1");
        let mut wrong_protocol = route.clone();
        wrong_protocol["protocol"] = serde_json::json!("open_ai_chat");
        let mut wrong_quota = route.clone();
        wrong_quota["max_output_tokens"] = serde_json::json!(4096);
        let mut wrong_model = route;
        wrong_model["model"] = serde_json::json!("arbitrary-model");

        for (route, credential, purposes) in [
            (
                wrong_endpoint,
                openai_oauth(unix_now_seconds() + 3600),
                serde_json::json!(["s2_planning"]),
            ),
            (
                wrong_protocol,
                openai_oauth(unix_now_seconds() + 3600),
                serde_json::json!(["s2_planning"]),
            ),
            (
                wrong_quota,
                openai_oauth(unix_now_seconds() + 3600),
                serde_json::json!(["s2_planning"]),
            ),
            (
                wrong_model,
                openai_oauth(unix_now_seconds() + 3600),
                serde_json::json!(["s2_planning"]),
            ),
            (
                openai_oauth_route(),
                openai_oauth(unix_now_seconds().saturating_sub(1)),
                serde_json::json!(["s2_planning"]),
            ),
            (
                openai_oauth_route(),
                api_key("must-not-downgrade"),
                serde_json::json!(["s2_planning"]),
            ),
            (
                openai_oauth_route(),
                openai_oauth(unix_now_seconds() + 3600),
                serde_json::json!(["computer_proposals"]),
            ),
        ] {
            let payload =
                serde_json::json!({"purposes":purposes, "credential":credential, "route":route});
            assert!(
                ModelBootstrap::read(&mut &frame(&serde_json::to_vec(&payload).unwrap())[..])
                    .is_err(),
                "accepted invalid OAuth authorization"
            );
        }

        let mut extra_field = openai_oauth(unix_now_seconds() + 3600);
        extra_field["refresh_token"] = serde_json::json!("must-not-persist");
        let payload = serde_json::json!({
            "purposes":["s2_planning"], "credential":extra_field,
            "route":openai_oauth_route()
        });
        assert!(
            ModelBootstrap::read(&mut &frame(&serde_json::to_vec(&payload).unwrap())[..]).is_err()
        );
    }

    #[test]
    fn only_explicit_authorized_purposes_are_present() {
        for (purposes, s2, s1) in [
            (vec!["s2_planning"], true, false),
            (vec!["s1_teacher"], false, true),
            (vec!["s1_teacher", "s2_planning"], true, true),
        ] {
            let payload = serde_json::json!({"purposes":purposes,"credential":api_key("key"),"route":{
                "provider_id":"open_ai", "protocol":"open_ai_chat", "base_url":"https://example.invalid/v1",
                "model":"test-model", "max_output_tokens":4096}});
            let bootstrap =
                ModelBootstrap::read(&mut &frame(&serde_json::to_vec(&payload).unwrap())[..])
                    .unwrap();
            assert_eq!(bootstrap.authorizes(ModelPurpose::S2Planning), s2);
            assert_eq!(bootstrap.authorizes(ModelPurpose::S1Teacher), s1);
            assert!(!bootstrap.authorizes(ModelPurpose::ComputerProposals));
        }
    }

    #[test]
    fn model_services_only_construct_authorized_slots_without_making_calls() {
        for (purposes, s2, s1) in [
            (vec!["s2_planning"], true, false),
            (vec!["s1_teacher"], false, true),
            (vec!["s2_planning", "s1_teacher"], true, true),
        ] {
            let payload = serde_json::json!({"purposes":purposes,"credential":api_key("test-only-key"),"route":{
                "provider_id":"open_ai", "protocol":"open_ai_chat", "base_url":"https://example.invalid/v1",
                "model":"test-model", "max_output_tokens":4096}});
            let bootstrap =
                ModelBootstrap::read(&mut &frame(&serde_json::to_vec(&payload).unwrap())[..])
                    .unwrap();
            let services = bootstrap
                .into_services(SharedClock::starting_now())
                .unwrap();
            assert_eq!(services.planner.is_some(), s2);
            assert_eq!(services.teacher.is_some(), s1);
            assert!(services.computer.is_none());
            drop(services);
        }
    }

    #[test]
    fn computer_purpose_requires_a_scope_and_the_exact_official_route() {
        let valid_route = serde_json::json!({
            "provider_id":"open_ai", "protocol":"open_ai_responses", "base_url":"https://api.openai.com/v1",
            "model":"test-model", "max_output_tokens":4096
        });
        let valid = serde_json::json!({
            "purposes":["computer_proposals"], "account_scope":TEST_ACCOUNT_SCOPE,
            "credential":api_key("key"), "route":valid_route.clone()
        });
        let bootstrap =
            ModelBootstrap::read(&mut &frame(&serde_json::to_vec(&valid).unwrap())[..]).unwrap();
        assert!(bootstrap.authorizes(ModelPurpose::ComputerProposals));

        let missing_scope = serde_json::json!({"purposes":["computer_proposals"], "credential":api_key("key"), "route":valid_route.clone()});
        let invalid_scope = serde_json::json!({
            "purposes":["computer_proposals"], "account_scope":"key-derived-or-malformed",
            "credential":api_key("key"), "route":valid_route.clone()
        });
        let extra_scope = serde_json::json!({
            "purposes":["s2_planning"], "account_scope":TEST_ACCOUNT_SCOPE,
            "credential":api_key("key"), "route":valid_route.clone()
        });
        let wrong_protocol = serde_json::json!({
            "provider_id":"open_ai", "protocol":"open_ai_chat", "base_url":"https://api.openai.com/v1",
            "model":"test-model", "max_output_tokens":4096
        });
        let wrong_endpoint = serde_json::json!({
            "provider_id":"open_ai", "protocol":"open_ai_responses", "base_url":"https://example.invalid/v1",
            "model":"test-model", "max_output_tokens":4096
        });
        let wrong_provider = serde_json::json!({
            "provider_id":"anthropic_first_party", "protocol":"anthropic_messages", "base_url":"https://api.openai.com/v1",
            "model":"test-model", "max_output_tokens":4096
        });
        for (route, scope) in [
            (valid_route.clone(), None),
            (valid_route, Some("not-a-uuid")),
            (wrong_protocol, Some(TEST_ACCOUNT_SCOPE)),
            (wrong_endpoint, Some(TEST_ACCOUNT_SCOPE)),
            (wrong_provider, Some(TEST_ACCOUNT_SCOPE)),
        ] {
            let mut payload = serde_json::json!({
                "purposes":["computer_proposals"], "credential":api_key("key"), "route":route
            });
            if let Some(scope) = scope {
                payload["account_scope"] = serde_json::json!(scope);
            }
            assert!(
                ModelBootstrap::read(&mut &frame(&serde_json::to_vec(&payload).unwrap())[..])
                    .is_err()
            );
        }
        for payload in [missing_scope, invalid_scope, extra_scope] {
            assert!(
                ModelBootstrap::read(&mut &frame(&serde_json::to_vec(&payload).unwrap())[..])
                    .is_err()
            );
        }
    }

    #[test]
    fn computer_services_are_created_only_for_explicit_scopes_without_model_calls() {
        for purposes in [
            vec!["computer_proposals"],
            vec!["computer_proposals", "s2_planning", "s1_teacher"],
        ] {
            let payload = serde_json::json!({
                "purposes":purposes,
                "account_scope":TEST_ACCOUNT_SCOPE,
                "credential":api_key("test-only-key"),
                "route":{
                    "provider_id":"open_ai", "protocol":"open_ai_responses", "base_url":"https://api.openai.com/v1",
                    "model":"test-model", "max_output_tokens":4096
                }
            });
            let bootstrap =
                ModelBootstrap::read(&mut &frame(&serde_json::to_vec(&payload).unwrap())[..])
                    .unwrap();
            let services = bootstrap
                .into_services(SharedClock::starting_now())
                .unwrap();
            assert!(services.computer.is_some());
            assert_eq!(
                services.planner.is_some(),
                payload["purposes"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("s2_planning"))
            );
            assert_eq!(
                services.teacher.is_some(),
                payload["purposes"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("s1_teacher"))
            );
            drop(services);
        }
    }
}
