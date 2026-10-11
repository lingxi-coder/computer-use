//! Private helper operations for the SDK-owned built-in provider catalog.
//!
//! The catalog is display-only. Provider IDs, wire protocols, authentication
//! strategies, and model capability facts come from the pinned SDK presets;
//! refreshes use that profile's declared directory shape and the SDK probe.

use std::collections::BTreeSet;
use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use llm_runtime::{AuthStrategy, ProviderProfile};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{
    ModelProtocol, ModelProvider, model_is_decision_selectable, protocol_for_preset,
    provider_id_for_preset,
};

const MAX_CREDENTIAL_BYTES: usize = 16 * 1024;
const MAX_MODELS: usize = 4096;
const DIRECTORY_TIMEOUT: Duration = Duration::from_secs(30);
const CHATGPT_PROFILE: &str = "openai-chatgpt";

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelCredential {
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

impl fmt::Debug for ModelCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ModelCredential([REDACTED])")
    }
}

#[derive(Serialize)]
pub struct ProviderCatalogResponse {
    providers: Vec<ProviderDefinition>,
}

#[derive(Serialize)]
struct ProviderDefinition {
    id: String,
    label: String,
    description: String,
    route: ProviderRoute,
    auth: CatalogAuth,
    models: Vec<ProviderModel>,
    directory_supported: bool,
}

#[derive(Serialize)]
struct ProviderRoute {
    provider_id: ModelProvider,
    protocol: ModelProtocol,
    base_url: String,
    max_output_tokens: u32,
    preset_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CatalogAuth {
    ApiKey,
    #[serde(rename = "openai_oauth")]
    OpenAiOAuth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderModel {
    id: String,
    label: String,
    selectable: bool,
}

pub fn catalog() -> ProviderCatalogResponse {
    let providers = llm_runtime::builtin_presets()
        .providers
        .into_iter()
        .filter_map(provider_definition)
        .collect();
    ProviderCatalogResponse { providers }
}

pub async fn models(
    profile_name: &str,
    credential: &ModelCredential,
    transport: &dyn lingxi_llm_client::Transport,
) -> Result<Vec<ProviderModel>, ()> {
    let profile = trusted_profile(profile_name).ok_or(())?;
    let wire = profile.wire_profile.as_ref().ok_or(())?;
    let protocol = directory_protocol(&profile).ok_or(())?;
    let probe_credential = credential_for_profile(credential, &profile).ok_or(())?;
    let result = lingxi_llm_client::directory::probe::probe(
        transport,
        &profile.base_url,
        protocol,
        &probe_credential,
        DIRECTORY_TIMEOUT,
    )
    .await
    .map_err(|_| ())?;
    let ids = result.model_ids.ok_or(())?;
    if ids.len() > MAX_MODELS {
        return Err(());
    }

    let mut seen = BTreeSet::new();
    let mut output = Vec::with_capacity(ids.len());
    for id in ids {
        if !valid_text(&id, 256) {
            return Err(());
        }
        if !seen.insert(id.clone()) {
            continue;
        }
        let mut known_rows = wire
            .models
            .iter()
            .filter(|model| model.request_model == id && !model.hidden);
        let known = known_rows.next().filter(|_| known_rows.next().is_none());
        let label = known.map_or(id.as_str(), |model| model.display_model.as_str());
        output.push(ProviderModel {
            id: id.clone(),
            label: {
                let label = bounded_text(label, 256);
                if label.is_empty() { id.clone() } else { label }
            },
            selectable: model_is_decision_selectable(&profile, &id),
        });
    }
    Ok(output)
}

fn provider_definition(profile: ProviderProfile) -> Option<ProviderDefinition> {
    let wire = profile.wire_profile.as_ref()?;
    if !wire.chat_enabled {
        return None;
    }
    let id = profile.profile_name.clone();
    if !valid_profile_id(&id) {
        return None;
    }
    let provider_id = provider_id_for_preset(&profile.provider_id)?;
    let protocol = protocol_for_preset(&profile.protocol)?;
    if !valid_provider_id(&provider_id) || !valid_base_url(&profile.base_url) {
        return None;
    }
    let (auth, max_output_tokens) = match profile.auth {
        AuthStrategy::ApiKey | AuthStrategy::Bearer => (CatalogAuth::ApiKey, 4096),
        AuthStrategy::ChatGptOAuth
            if id == CHATGPT_PROFILE
                && profile.provider_id
                    == (llm_runtime::ProviderId::OpenAICompatible {
                        name: CHATGPT_PROFILE.into(),
                    })
                && profile.protocol == llm_runtime::ProtocolFamily::OpenAiResponses
                && profile.base_url == "https://chatgpt.com/backend-api/codex" =>
        {
            (CatalogAuth::OpenAiOAuth, 2048)
        }
        _ => return None,
    };
    let mut seen = BTreeSet::new();
    let models = wire
        .models
        .iter()
        .filter(|model| !model.hidden)
        .filter(|model| seen.insert(model.request_model.clone()))
        .filter(|model| valid_text(&model.request_model, 256))
        .map(|model| ProviderModel {
            id: model.request_model.clone(),
            label: {
                let label = bounded_text(&model.display_model, 256);
                if label.is_empty() {
                    model.request_model.clone()
                } else {
                    label
                }
            },
            selectable: model_is_decision_selectable(&profile, &model.request_model),
        })
        .collect();
    let info = &wire.info;
    let label = info
        .display_name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&id);
    let label = bounded_text(label, 256);
    let directory_supported = directory_protocol(&profile).is_some();
    Some(ProviderDefinition {
        id: id.clone(),
        label: if label.is_empty() { id.clone() } else { label },
        description: info
            .description
            .as_deref()
            .map(|value| bounded_text(value, 2048))
            .unwrap_or_default(),
        route: ProviderRoute {
            provider_id,
            protocol,
            base_url: profile.base_url.clone(),
            max_output_tokens,
            preset_id: id.clone(),
        },
        auth,
        models,
        directory_supported,
    })
}

fn trusted_profile(profile_name: &str) -> Option<ProviderProfile> {
    let mut profiles = llm_runtime::builtin_presets()
        .providers
        .into_iter()
        .filter(|profile| profile.profile_name == profile_name)
        .filter(|profile| provider_definition(profile.clone()).is_some());
    let profile = profiles.next()?;
    profiles.next().is_none().then_some(profile)
}

fn directory_protocol(
    profile: &ProviderProfile,
) -> Option<lingxi_llm_client::directory::probe::ProbeProtocol> {
    let wire = profile.wire_profile.as_ref()?;
    if !wire.chat_enabled {
        return None;
    }
    let shape = wire.model_list.shape(wire.protocol)?;
    if profile.auth == AuthStrategy::ChatGptOAuth {
        return (profile.profile_name == CHATGPT_PROFILE
            && shape == llm_runtime::ProtocolFamily::OpenAiChat)
            .then_some(lingxi_llm_client::directory::probe::ProbeProtocol::ChatGpt);
    }
    Some(match shape {
        llm_runtime::ProtocolFamily::AnthropicMessages => {
            lingxi_llm_client::directory::probe::ProbeProtocol::Anthropic
        }
        llm_runtime::ProtocolFamily::OpenAiChat => {
            lingxi_llm_client::directory::probe::ProbeProtocol::OpenAi
        }
        llm_runtime::ProtocolFamily::GeminiGenerateContent => {
            lingxi_llm_client::directory::probe::ProbeProtocol::Google
        }
        _ => return None,
    })
}

fn credential_for_profile(
    credential: &ModelCredential,
    profile: &ProviderProfile,
) -> Option<lingxi_llm_client::directory::probe::ProbeCredential> {
    use lingxi_llm_client::directory::probe::ProbeCredential;
    use lingxi_llm_client::protocol::Secret;

    match (credential, profile.auth) {
        (ModelCredential::ApiKey { api_key }, AuthStrategy::ApiKey | AuthStrategy::Bearer)
            if !api_key.is_empty()
                && api_key.len() <= MAX_CREDENTIAL_BYTES
                && !api_key.chars().any(char::is_control) =>
        {
            Some(ProbeCredential {
                token: Secret::new(api_key.to_string()),
                bearer: profile.auth == AuthStrategy::Bearer,
                account_id: None,
                fedramp: false,
            })
        }
        (
            ModelCredential::OpenAiOAuth {
                access_token,
                expires_at,
                account_id,
                fedramp,
            },
            AuthStrategy::ChatGptOAuth,
        ) if profile.profile_name == CHATGPT_PROFILE
            && !access_token.is_empty()
            && access_token.len() <= MAX_CREDENTIAL_BYTES
            && !access_token.chars().any(char::is_control)
            && *expires_at > unix_now_seconds()
            && account_id.as_ref().is_none_or(|account| {
                !account.is_empty()
                    && account.len() <= 512
                    && !account.chars().any(char::is_control)
            }) =>
        {
            Some(ProbeCredential {
                token: Secret::new(access_token.to_string()),
                bearer: true,
                account_id: account_id.clone(),
                fedramp: *fedramp,
            })
        }
        _ => None,
    }
}

fn valid_profile_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn valid_provider_id(provider: &ModelProvider) -> bool {
    match provider {
        ModelProvider::AnthropicFirstParty | ModelProvider::OpenAi | ModelProvider::Gemini => true,
        ModelProvider::OpenAiCompatible { name } | ModelProvider::Custom { name } => {
            valid_text(name, 128)
        }
    }
}

fn valid_base_url(value: &str) -> bool {
    if value.len() > 2048 {
        return false;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn valid_text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn bounded_text(value: &str, limit: usize) -> String {
    let mut output = String::new();
    let mut whitespace = false;
    for character in value.chars() {
        if character.is_control() || character.is_whitespace() {
            whitespace = !output.is_empty();
            continue;
        }
        let separator = whitespace.then_some(' ');
        let needed = character.len_utf8() + usize::from(separator.is_some());
        if output.len() + needed > limit {
            break;
        }
        if let Some(separator) = separator {
            output.push(separator);
        }
        output.push(character);
        whitespace = false;
    }
    output
}

fn unix_now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::StreamExt;
    use lingxi_llm_client::{HttpRequest, StreamResponse, Transport, protocol::LlmError};
    use std::sync::Mutex;

    fn route_for(provider: &ProviderDefinition, model: &ProviderModel) -> super::super::ModelRoute {
        super::super::ModelRoute {
            preset_id: Some(provider.route.preset_id.clone()),
            provider_id: provider.route.provider_id.clone(),
            protocol: provider.route.protocol,
            base_url: provider.route.base_url.clone(),
            model: model.id.clone(),
            max_output_tokens: provider.route.max_output_tokens,
        }
    }

    struct OnePage {
        request: Mutex<Option<HttpRequest>>,
        body: serde_json::Value,
    }

    #[async_trait]
    impl Transport for OnePage {
        async fn send(&self, request: HttpRequest) -> Result<StreamResponse, LlmError> {
            *self.request.lock().unwrap() = Some(request);
            let body = self.body.to_string();
            Ok(StreamResponse {
                status: 200,
                headers: vec![],
                body: futures::stream::once(async move { Ok(body.into()) }).boxed(),
            })
        }
    }

    #[test]
    fn catalog_contains_only_supported_direct_auth_presets_and_gemini() {
        let value = serde_json::to_value(catalog()).unwrap();
        let providers = value["providers"].as_array().unwrap();
        assert!(providers.iter().any(|row| {
            row["id"] == "gemini"
                && row["route"]["provider_id"] == "gemini"
                && row["route"]["protocol"] == "gemini_generate_content"
                && row["auth"] == "api_key"
        }));
        assert!(providers.iter().any(|row| {
            row["id"] == CHATGPT_PROFILE
                && row["auth"] == "openai_oauth"
                && row["route"]["max_output_tokens"] == 2048
        }));
        assert!(providers.iter().all(|row| {
            matches!(row["auth"].as_str(), Some("api_key" | "openai_oauth"))
                && row["route"]["preset_id"] == row["id"]
                && row["models"].as_array().is_some()
        }));
    }

    #[test]
    fn every_catalog_route_validates_with_a_known_selectable_sdk_model() {
        for provider in catalog().providers {
            let model = provider
                .models
                .iter()
                .find(|model| model.selectable)
                .unwrap_or_else(|| panic!("{} has no selectable SDK model", provider.id));
            let route = route_for(&provider, model);
            assert!(
                route.validate().is_ok(),
                "trusted preset route failed validation: {}",
                provider.id
            );
        }
    }

    #[test]
    fn only_exact_compatible_anthropic_presets_receive_the_narrow_route_exception() {
        const PRESETS: [&str; 6] = [
            "deepseek-search",
            "glm-coding",
            "grok-anthropic",
            "minimax",
            "minimax-intl",
            "zai-coding",
        ];
        let catalog = catalog();
        for preset in PRESETS {
            let provider = catalog
                .providers
                .iter()
                .find(|provider| provider.id == preset)
                .unwrap_or_else(|| panic!("missing SDK preset {preset}"));
            assert_eq!(provider.route.protocol, ModelProtocol::AnthropicMessages);
            assert!(matches!(
                &provider.route.provider_id,
                ModelProvider::OpenAiCompatible { .. }
            ));
            let model = provider
                .models
                .iter()
                .find(|model| model.selectable)
                .unwrap_or_else(|| panic!("{preset} has no selectable model"));
            let route = route_for(provider, model);
            assert!(route.validate().is_ok());

            let mut generic = route.clone();
            generic.preset_id = None;
            assert!(generic.validate().is_err());

            let mut wrong_endpoint = route;
            wrong_endpoint.base_url = "https://attacker.example/v1".into();
            assert!(wrong_endpoint.validate().is_err());
        }
    }

    #[test]
    fn api_key_and_oauth_material_cannot_cross_provider_auth_boundaries() {
        let profiles = llm_runtime::builtin_presets().providers;
        let gemini = profiles
            .iter()
            .find(|p| p.profile_name == "gemini")
            .unwrap();
        let chatgpt = profiles
            .iter()
            .find(|p| p.profile_name == CHATGPT_PROFILE)
            .unwrap();
        let api_key: ModelCredential = serde_json::from_value(serde_json::json!({
            "kind":"api_key", "api_key":"token"
        }))
        .unwrap();
        let oauth: ModelCredential = serde_json::from_value(serde_json::json!({
            "kind":"openai_oauth", "access_token":"token",
            "expires_at":unix_now_seconds() + 3600,"fedramp":false
        }))
        .unwrap();
        assert!(credential_for_profile(&api_key, gemini).is_some());
        assert!(credential_for_profile(&oauth, gemini).is_none());
        assert!(credential_for_profile(&api_key, chatgpt).is_none());
        assert!(credential_for_profile(&oauth, chatgpt).is_some());
    }

    #[test]
    fn credential_union_rejects_unknown_fields_and_never_formats_secrets() {
        assert!(
            serde_json::from_value::<ModelCredential>(serde_json::json!({
                "kind":"api_key", "api_key":"secret", "bearer":true
            }))
            .is_err()
        );
        let credential: ModelCredential = serde_json::from_value(serde_json::json!({
            "kind":"api_key", "api_key":"private-catalog-key"
        }))
        .unwrap();
        assert!(!format!("{credential:?}").contains("private-catalog-key"));
    }

    #[tokio::test]
    async fn gemini_refresh_uses_sdk_directory_and_api_key_auth() {
        let profile = trusted_profile("gemini").unwrap();
        let wire = profile.wire_profile.as_ref().unwrap();
        let known = wire
            .models
            .iter()
            .find(|model| !model.hidden)
            .unwrap()
            .request_model
            .clone();
        let transport = OnePage {
            request: Mutex::new(None),
            body: serde_json::json!({
                "models":[
                    {"name":format!("models/{known}"),"supportedGenerationMethods":["generateContent"]},
                    {"name":"models/not-in-the-sdk-catalog","supportedGenerationMethods":["generateContent"]}
                ]
            }),
        };
        let credential: ModelCredential = serde_json::from_value(serde_json::json!({
            "kind":"api_key", "api_key":"private-google-key"
        }))
        .unwrap();
        let models = models("gemini", &credential, &transport).await.unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, known);
        assert!(!models[1].selectable);
        let request = transport.request.lock().unwrap();
        let request = request.as_ref().unwrap();
        assert!(request.url.contains("/models?pageSize=1000"));
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("x-goog-api-key")
                    && value == "private-google-key")
        );
    }
}
