use std::{
    collections::BTreeMap,
    fmt,
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{Deserialize, Serialize};

pub trait ModelProvider {
    fn complete(&self, request: ModelRequest) -> anyhow::Result<ModelResponse>;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelRequest {
    pub purpose: ModelPurpose,
    pub prompt: String,
    pub estimated_tokens: EstimatedTokenUsage,
    pub max_output_tokens: Option<usize>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelResponse {
    pub text: String,
    pub reported_tokens: ReportedTokenUsage,
    pub finish_reason: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelCallRecord {
    pub purpose: ModelPurpose,
    pub estimated_tokens: EstimatedTokenUsage,
    pub reported_tokens: ReportedTokenUsage,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EstimatedTokenUsage {
    pub input: usize,
    pub output: usize,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReportedTokenUsage {
    pub input: Option<usize>,
    pub output: Option<usize>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelPurpose {
    LogSummary,
    ContextRanking,
    PatchGeneration,
    PatchReview,
    FinalReport,
}

impl ModelCallRecord {
    pub fn from_request_response(request: &ModelRequest, response: &ModelResponse) -> Self {
        Self {
            purpose: request.purpose,
            estimated_tokens: request.estimated_tokens,
            reported_tokens: response.reported_tokens,
        }
    }
}

impl fmt::Display for ModelPurpose {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::LogSummary => "log_summary",
            Self::ContextRanking => "context_ranking",
            Self::PatchGeneration => "patch_generation",
            Self::PatchReview => "patch_review",
            Self::FinalReport => "final_report",
        })
    }
}

impl FromStr for ModelPurpose {
    type Err = ParseModelPurposeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "log_summary" => Ok(Self::LogSummary),
            "context_ranking" => Ok(Self::ContextRanking),
            "patch_generation" => Ok(Self::PatchGeneration),
            "patch_review" => Ok(Self::PatchReview),
            "final_report" => Ok(Self::FinalReport),
            _ => Err(ParseModelPurposeError),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseModelPurposeError;

impl fmt::Display for ParseModelPurposeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("unknown model purpose")
    }
}

impl std::error::Error for ParseModelPurposeError {}

/// Append-only ledger of every model call made through a [`ModelProvider`].
///
/// Cheaply cloneable: all clones share the same underlying log.
#[derive(Clone, Debug, Default)]
pub struct ModelLedger {
    records: Arc<Mutex<Vec<ModelCallRecord>>>,
}

impl ModelLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, entry: ModelCallRecord) {
        self.records
            .lock()
            .expect("model ledger mutex poisoned")
            .push(entry);
    }

    pub fn entries(&self) -> Vec<ModelCallRecord> {
        self.records
            .lock()
            .expect("model ledger mutex poisoned")
            .clone()
    }

    pub fn len(&self) -> usize {
        self.records
            .lock()
            .expect("model ledger mutex poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// OpenAI-compatible chat completions provider.
///
/// Reads the API key from the environment at call time (never stored in the
/// struct). Every attempt — successful or not — is appended to `ledger` so
/// trace data is never silently lost.
pub struct OpenAiProvider {
    config: crate::config::ModelConfig,
    pub ledger: ModelLedger,
}

impl OpenAiProvider {
    pub fn new(config: crate::config::ModelConfig) -> Self {
        Self {
            config,
            ledger: ModelLedger::new(),
        }
    }

    fn api_key(&self) -> anyhow::Result<String> {
        std::env::var(&self.config.api_key_env_var).map_err(|_| {
            anyhow::anyhow!(
                "environment variable '{}' is not set; \
                 set it to the API key for {}",
                self.config.api_key_env_var,
                self.config.base_url
            )
        })
    }
}

impl ModelProvider for OpenAiProvider {
    fn complete(&self, request: ModelRequest) -> anyhow::Result<ModelResponse> {
        let api_key = self.api_key()?;
        let url = format!("{}/chat/completions", self.config.base_url);

        let body = OpenAiChatRequest {
            model: &self.config.model,
            messages: vec![OpenAiMessage {
                role: "user",
                content: &request.prompt,
            }],
            max_tokens: request.max_output_tokens,
        };

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(self.config.timeout_secs))
            .build();

        let http_result = agent
            .post(&url)
            .set("Authorization", &format!("Bearer {api_key}"))
            .set("Content-Type", "application/json")
            .send_json(serde_json::to_value(&body)?);

        let (response, reported) = match http_result {
            Ok(http_response) => {
                let raw: OpenAiChatResponse = http_response
                    .into_json()
                    .map_err(|e| anyhow::anyhow!("invalid JSON from model API: {e}"))?;

                let reported = ReportedTokenUsage {
                    input: raw.usage.as_ref().map(|u| u.prompt_tokens),
                    output: raw.usage.as_ref().map(|u| u.completion_tokens),
                };
                let finish_reason = raw.choices.first().and_then(|c| c.finish_reason.clone());

                let text = raw
                    .choices
                    .into_iter()
                    .next()
                    .and_then(|c| c.message)
                    .map(|m| m.content)
                    .ok_or_else(|| anyhow::anyhow!("model API returned no choices"))?;

                let response = ModelResponse {
                    text,
                    reported_tokens: reported,
                    finish_reason,
                    metadata: BTreeMap::new(),
                };
                (response, reported)
            }
            Err(ureq::Error::Status(status, http_response)) => {
                let body_snippet = http_response.into_string().unwrap_or_default();
                let trimmed = body_snippet.chars().take(256).collect::<String>();
                let reported = ReportedTokenUsage::default();
                self.ledger.record(ModelCallRecord {
                    purpose: request.purpose,
                    estimated_tokens: request.estimated_tokens,
                    reported_tokens: reported,
                });
                return Err(anyhow::anyhow!(
                    "model API returned HTTP {status} for {} ({}/{}): {trimmed}",
                    request.purpose,
                    self.config.base_url,
                    self.config.model
                ));
            }
            Err(ureq::Error::Transport(transport)) => {
                let reported = ReportedTokenUsage::default();
                self.ledger.record(ModelCallRecord {
                    purpose: request.purpose,
                    estimated_tokens: request.estimated_tokens,
                    reported_tokens: reported,
                });
                return Err(anyhow::anyhow!(
                    "transport error calling model API ({}/{}): {transport}",
                    self.config.base_url,
                    self.config.model
                ));
            }
        };

        self.ledger.record(ModelCallRecord {
            purpose: request.purpose,
            estimated_tokens: request.estimated_tokens,
            reported_tokens: reported,
        });

        Ok(response)
    }
}

// ── OpenAI wire types (private) ──────────────────────────────────────────────

#[derive(Serialize)]
struct OpenAiChatRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<usize>,
}

#[derive(Serialize)]
struct OpenAiMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: Option<OpenAiChoiceMessage>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiChoiceMessage {
    content: String,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    prompt_tokens: usize,
    completion_tokens: usize,
}

/// Mock provider that returns pre-loaded responses and records every call in a
/// shared [`ModelLedger`]. No network access is required.
///
/// Responses are consumed in FIFO order. If the queue is exhausted the provider
/// returns an error, making missing-stub scenarios explicit in tests.
#[cfg(any(test, feature = "test-utils"))]
pub struct MockModelProvider {
    responses: Mutex<Vec<ModelResponse>>,
    pub ledger: ModelLedger,
}

#[cfg(any(test, feature = "test-utils"))]
impl MockModelProvider {
    /// Create a provider pre-loaded with `responses`. They are returned in the
    /// order given.
    pub fn with_responses(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            ledger: ModelLedger::new(),
        }
    }

    /// Convenience constructor for tests that only need a single fixed reply.
    pub fn fixed(text: impl Into<String>) -> Self {
        Self::with_responses(vec![ModelResponse {
            text: text.into(),
            reported_tokens: ReportedTokenUsage::default(),
            finish_reason: Some("stop".to_string()),
            metadata: BTreeMap::new(),
        }])
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl ModelProvider for MockModelProvider {
    fn complete(&self, request: ModelRequest) -> anyhow::Result<ModelResponse> {
        let response = self
            .responses
            .lock()
            .expect("mock provider mutex poisoned")
            .pop()
            .ok_or_else(|| anyhow::anyhow!("MockModelProvider: no more staged responses"))?;

        self.ledger
            .record(ModelCallRecord::from_request_response(&request, &response));

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request(purpose: ModelPurpose) -> ModelRequest {
        ModelRequest {
            purpose,
            prompt: "test prompt".to_string(),
            estimated_tokens: EstimatedTokenUsage {
                input: 42,
                output: 12,
            },
            max_output_tokens: Some(64),
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn serializes_model_purposes_as_stable_snake_case() {
        assert_eq!(ModelPurpose::LogSummary.to_string(), "log_summary");
        assert_eq!(ModelPurpose::ContextRanking.to_string(), "context_ranking");
        assert_eq!(
            ModelPurpose::PatchGeneration.to_string(),
            "patch_generation"
        );
        assert_eq!(ModelPurpose::PatchReview.to_string(), "patch_review");
        assert_eq!(ModelPurpose::FinalReport.to_string(), "final_report");

        let rendered = serde_json::to_string(&ModelPurpose::PatchReview).unwrap();
        assert_eq!(rendered, "\"patch_review\"");
    }

    #[test]
    fn parses_known_model_purposes() {
        assert_eq!(
            "context_ranking".parse::<ModelPurpose>(),
            Ok(ModelPurpose::ContextRanking)
        );
        assert_eq!(
            "unknown".parse::<ModelPurpose>(),
            Err(ParseModelPurposeError)
        );
    }

    #[test]
    fn records_estimated_and_reported_tokens_for_model_calls() {
        let request = ModelRequest {
            purpose: ModelPurpose::FinalReport,
            prompt: "summarize this run".to_string(),
            estimated_tokens: EstimatedTokenUsage {
                input: 42,
                output: 12,
            },
            max_output_tokens: Some(64),
            metadata: BTreeMap::new(),
        };
        let response = ModelResponse {
            text: "done".to_string(),
            reported_tokens: ReportedTokenUsage {
                input: Some(40),
                output: Some(8),
            },
            finish_reason: Some("stop".to_string()),
            metadata: BTreeMap::new(),
        };

        let record = ModelCallRecord::from_request_response(&request, &response);

        assert_eq!(record.purpose, ModelPurpose::FinalReport);
        assert_eq!(record.estimated_tokens.input, 42);
        assert_eq!(record.reported_tokens.output, Some(8));
    }

    #[test]
    fn mock_provider_returns_fixed_response_without_network() {
        let provider = MockModelProvider::fixed("patch applied");
        let request = sample_request(ModelPurpose::PatchGeneration);

        let response = provider.complete(request).expect("mock should not fail");

        assert_eq!(response.text, "patch applied");
        assert_eq!(response.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn mock_provider_records_calls_in_ledger() {
        let provider = MockModelProvider::with_responses(vec![
            ModelResponse {
                text: "summary".to_string(),
                reported_tokens: ReportedTokenUsage {
                    input: Some(10),
                    output: Some(4),
                },
                finish_reason: Some("stop".to_string()),
                metadata: BTreeMap::new(),
            },
            ModelResponse {
                text: "ranking".to_string(),
                reported_tokens: ReportedTokenUsage::default(),
                finish_reason: Some("stop".to_string()),
                metadata: BTreeMap::new(),
            },
        ]);

        provider
            .complete(sample_request(ModelPurpose::ContextRanking))
            .unwrap();
        provider
            .complete(sample_request(ModelPurpose::LogSummary))
            .unwrap();

        let entries = provider.ledger.entries();
        assert_eq!(entries.len(), 2);
        // responses are consumed LIFO from the Vec; verify each purpose landed
        let purposes: Vec<ModelPurpose> = entries.iter().map(|e| e.purpose).collect();
        assert!(purposes.contains(&ModelPurpose::ContextRanking));
        assert!(purposes.contains(&ModelPurpose::LogSummary));
    }

    #[test]
    fn mock_provider_ledger_captures_estimated_tokens() {
        let provider = MockModelProvider::fixed("ok");
        let mut request = sample_request(ModelPurpose::PatchReview);
        request.estimated_tokens = EstimatedTokenUsage {
            input: 100,
            output: 20,
        };

        provider.complete(request).unwrap();

        let entries = provider.ledger.entries();
        assert_eq!(entries[0].estimated_tokens.input, 100);
        assert_eq!(entries[0].estimated_tokens.output, 20);
    }

    #[test]
    fn mock_provider_errors_when_responses_exhausted() {
        let provider = MockModelProvider::with_responses(vec![]);

        let result = provider.complete(sample_request(ModelPurpose::FinalReport));

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no more staged responses")
        );
    }

    #[test]
    fn ledger_clones_share_the_same_log() {
        let provider = MockModelProvider::fixed("shared");
        let ledger_clone = provider.ledger.clone();

        provider
            .complete(sample_request(ModelPurpose::LogSummary))
            .unwrap();

        assert_eq!(ledger_clone.len(), 1);
        assert_eq!(provider.ledger.len(), 1);
    }
}
