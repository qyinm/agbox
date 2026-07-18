//! Optional, bounded semantic refinement.
//!
//! Refinement is disabled unless a caller explicitly supplies a validated
//! loopback URL.  This module never executes a proposed action and never
//! sends source paths, reasoning, or unrestricted tool output over HTTP.

use std::net::IpAddr;
use std::time::Duration;

use agbox_core::{
    Authority, DisclosureClass, EventId, EvidenceId, PrivacyLabel, RedactionPolicy,
    WorkContractRevision,
};
use futures::StreamExt;
use reqwest::redirect::Policy;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::authority::SemanticPolicy;
use crate::{ContractField, ProvisionalContract};

pub const MAX_EXTRACTION_INPUT_BYTES: usize = 256 * 1024;
pub const MAX_ASSERTIONS_PER_RUN: usize = 64;
pub const MAX_ASSERTION_VALUE_BYTES: usize = 8 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    #[error("semantic extraction is disabled")]
    Disabled,
    #[error("semantic endpoint is not loopback HTTP")]
    EndpointDenied,
    #[error("semantic endpoint URL is invalid")]
    InvalidEndpoint(#[from] url::ParseError),
    #[error("semantic HTTP client could not be configured")]
    Client,
    #[error("semantic request failed")]
    Request,
    #[error("semantic response exceeded its bound")]
    ResponseTooLarge,
    #[error("semantic response schema is invalid")]
    InvalidResponse,
    #[error("semantic response contains too many assertions")]
    TooManyAssertions,
    #[error("semantic assertion is invalid")]
    InvalidProposal,
    #[error("semantic input exceeds its byte bound")]
    InputTooLarge,
    #[error("semantic input serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("semantic output redaction failed")]
    Redaction(#[from] agbox_core::RedactionError),
}

/// Explicitly configured loopback-only HTTP endpoint.
#[derive(Clone, Debug, Default)]
pub struct EndpointPolicy(Option<Url>);

impl EndpointPolicy {
    /// Parses an endpoint, accepting only literal loopback IP addresses over
    /// cleartext HTTP. Hostnames are rejected to avoid DNS and proxy egress.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidEndpoint`] for malformed URLs or
    /// [`SemanticError::EndpointDenied`] for any non-loopback endpoint.
    pub fn parse(value: &str) -> Result<Self, SemanticError> {
        let url = Url::parse(value)?;
        let allowed_host = match url.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            _ => false,
        };
        if url.scheme() != "http"
            || !allowed_host
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(SemanticError::EndpointDenied);
        }
        Ok(Self(Some(url)))
    }

    #[must_use]
    pub fn endpoint(&self) -> Option<&Url> {
        self.0.as_ref()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BoundedFact {
    pub evidence_id: EvidenceId,
    pub authority: Authority,
    pub disclosure_class: DisclosureClass,
    pub privacy: PrivacyLabel,
    pub text: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BoundedEvidence {
    pub evidence_id: EvidenceId,
    pub authority: Authority,
    pub disclosure_class: DisclosureClass,
    pub privacy: PrivacyLabel,
    pub excerpt: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct BoundedArtifact {
    pub artifact_id: String,
    pub state: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExtractionInput {
    pub previous_contract: WorkContractRevision,
    pub new_facts: Vec<BoundedFact>,
    pub evidence_excerpts: Vec<BoundedEvidence>,
    pub artifact_state: Vec<BoundedArtifact>,
}

impl ExtractionInput {
    /// Creates an input while retaining the previous contract and newest facts
    /// first. Restricted evidence and unbounded values are omitted.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::Serialization`] when the bounded input cannot
    /// be encoded, or [`SemanticError::InputTooLarge`] when the previous
    /// contract alone exceeds the request budget.
    pub fn bounded(
        previous_contract: WorkContractRevision,
        mut new_facts: Vec<BoundedFact>,
        mut evidence_excerpts: Vec<BoundedEvidence>,
        mut artifact_state: Vec<BoundedArtifact>,
    ) -> Result<Self, SemanticError> {
        new_facts.reverse();
        new_facts.retain(|fact| {
            fact.privacy != PrivacyLabel::RestrictedLocal
                && !fact.text.as_deref().is_some_and(contains_absolute_path)
                && !matches!(
                    fact.disclosure_class,
                    DisclosureClass::Reasoning
                        | DisclosureClass::SystemInstruction
                        | DisclosureClass::DeveloperInstruction
                )
        });
        evidence_excerpts.retain(|evidence| {
            evidence.disclosure_class != DisclosureClass::Reasoning
                && evidence.disclosure_class != DisclosureClass::SystemInstruction
                && evidence.disclosure_class != DisclosureClass::DeveloperInstruction
                && evidence.privacy != PrivacyLabel::RestrictedLocal
                && !contains_absolute_path(&evidence.excerpt)
        });
        let mut input = Self {
            previous_contract,
            new_facts: Vec::new(),
            evidence_excerpts: Vec::new(),
            artifact_state: Vec::new(),
        };
        let mut bytes = serde_json::to_vec(&input)?.len();
        for fact in new_facts {
            let candidate = serde_json::to_vec(&fact)?;
            if bytes.saturating_add(candidate.len()) > MAX_EXTRACTION_INPUT_BYTES {
                break;
            }
            bytes = bytes.saturating_add(candidate.len());
            input.new_facts.push(fact);
        }
        for evidence in evidence_excerpts {
            let candidate = serde_json::to_vec(&evidence)?;
            if bytes.saturating_add(candidate.len()) > MAX_EXTRACTION_INPUT_BYTES {
                break;
            }
            bytes = bytes.saturating_add(candidate.len());
            input.evidence_excerpts.push(evidence);
        }
        for artifact in artifact_state.drain(..) {
            let candidate = serde_json::to_vec(&artifact)?;
            if bytes.saturating_add(candidate.len()) > MAX_EXTRACTION_INPUT_BYTES {
                break;
            }
            bytes = bytes.saturating_add(candidate.len());
            input.artifact_state.push(artifact);
        }
        if serde_json::to_vec(&input)?.len() > MAX_EXTRACTION_INPUT_BYTES {
            return Err(SemanticError::InputTooLarge);
        }
        Ok(input)
    }
}

fn contains_absolute_path(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with("~/")
        || value.as_bytes().get(1).is_some_and(|byte| *byte == b':')
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedAssertions {
    pub assertions: Vec<ProposedAssertion>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedAssertion {
    pub field: String,
    pub value: String,
    pub authority: Authority,
    pub evidence_refs: Vec<EvidenceId>,
    pub confidence_basis_points: u16,
}

impl ProposedAssertions {
    pub(crate) fn validate_shape(&self) -> Result<(), SemanticError> {
        if self.assertions.len() > MAX_ASSERTIONS_PER_RUN {
            return Err(SemanticError::TooManyAssertions);
        }
        if self.assertions.iter().any(|assertion| {
            assertion.value.len() > MAX_ASSERTION_VALUE_BYTES
                || assertion.confidence_basis_points > 10_000
                || assertion.evidence_refs.len() > agbox_core::limits::MAX_CONTRACT_EVIDENCE_REFS
        }) {
            return Err(SemanticError::InvalidProposal);
        }
        Ok(())
    }
}

#[async_trait::async_trait]
pub trait SemanticExtractor: Send + Sync {
    fn version(&self) -> &str;

    async fn extract(&self, input: ExtractionInput) -> Result<ProposedAssertions, SemanticError>;
}

#[derive(Clone, Debug, Default)]
pub struct DisabledExtractor;

#[async_trait::async_trait]
impl SemanticExtractor for DisabledExtractor {
    #[allow(clippy::unnecessary_literal_bound)]
    fn version(&self) -> &str {
        "disabled-v1"
    }

    async fn extract(&self, _input: ExtractionInput) -> Result<ProposedAssertions, SemanticError> {
        Err(SemanticError::Disabled)
    }
}

#[derive(Clone, Debug)]
pub struct LoopbackExtractor {
    endpoint: Url,
    version: String,
    client: reqwest::Client,
}

impl LoopbackExtractor {
    /// Constructs an extractor only after endpoint policy validation.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::EndpointDenied`] when the policy is disabled
    /// or not loopback-only, or [`SemanticError::Client`] when the bounded
    /// HTTP client cannot be configured.
    pub fn new(policy: EndpointPolicy, version: impl Into<String>) -> Result<Self, SemanticError> {
        let endpoint = policy.0.ok_or(SemanticError::EndpointDenied)?;
        if endpoint.host().and_then(|host| match host {
            url::Host::Ipv4(value) => Some(IpAddr::V4(value).is_loopback()),
            url::Host::Ipv6(value) => Some(IpAddr::V6(value).is_loopback()),
            url::Host::Domain(_) => None,
        }) != Some(true)
        {
            return Err(SemanticError::EndpointDenied);
        }
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| SemanticError::Client)?;
        Ok(Self {
            endpoint,
            version: version.into(),
            client,
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }
}

#[async_trait::async_trait]
impl SemanticExtractor for LoopbackExtractor {
    fn version(&self) -> &str {
        &self.version
    }

    async fn extract(&self, input: ExtractionInput) -> Result<ProposedAssertions, SemanticError> {
        let body = serde_json::to_vec(&input)?;
        if body.len() > MAX_EXTRACTION_INPUT_BYTES {
            return Err(SemanticError::InputTooLarge);
        }
        let response = self
            .client
            .post(self.endpoint.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| SemanticError::Request)?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(SemanticError::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| SemanticError::Request)?;
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(SemanticError::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        let proposals: ProposedAssertions =
            serde_json::from_slice(&bytes).map_err(|_| SemanticError::InvalidResponse)?;
        proposals.validate_shape()?;
        Ok(proposals)
    }
}

/// Applies local immutable evidence rules to model output.
///
/// # Errors
///
/// Returns a bounded schema or policy error when the response exceeds its
/// assertion, evidence, value, or confidence limits.
pub fn filter_proposals(
    policy: &SemanticPolicy,
    proposals: ProposedAssertions,
) -> Result<ProposedAssertions, SemanticError> {
    policy.validate(proposals)
}

/// Applies authority-filtered, summary-only refinement to a provisional
/// contract. Instruction fields remain unchanged unless the local policy has
/// already proven an exact human-intent match. The returned value is pure and
/// still requires store validation before publication.
///
/// # Errors
///
/// Returns [`SemanticError::InvalidProposal`] when the response shape or
/// revision counter is invalid.
pub fn refine_provisional_contract(
    previous: &ProvisionalContract,
    proposals: &ProposedAssertions,
    extractor_version: impl Into<String>,
) -> Result<ProvisionalContract, SemanticError> {
    refine_provisional_contract_at(previous, proposals, extractor_version, previous.created_at)
}

/// As [`refine_provisional_contract`] but stamps the new immutable revision
/// with the extractor observation time.
///
/// # Errors
///
/// Returns [`SemanticError::InvalidProposal`] when the response shape or
/// revision counter is invalid, or [`SemanticError::Serialization`] when the
/// material digest cannot be encoded.
pub fn refine_provisional_contract_at(
    previous: &ProvisionalContract,
    proposals: &ProposedAssertions,
    extractor_version: impl Into<String>,
    observed_at: time::OffsetDateTime,
) -> Result<ProvisionalContract, SemanticError> {
    proposals.validate_shape()?;
    let mut refined = previous.clone();
    refined.revision = previous
        .revision
        .checked_add(1)
        .ok_or(SemanticError::InvalidProposal)?;
    refined.extractor_version = extractor_version.into();
    refined.created_at = observed_at;
    if let Some(summary) = proposals
        .assertions
        .iter()
        .find(|assertion| assertion.field.trim().eq_ignore_ascii_case("summary"))
    {
        let redaction = RedactionPolicy::new()?;
        let redacted = redaction.redact(&summary.value, None, DisclosureClass::DerivedText)?;
        redacted.value().clone_into(&mut refined.summary);
        let evidence: Vec<EventId> = summary
            .evidence_refs
            .iter()
            .filter_map(|evidence_id| EventId::parse_wire(evidence_id.as_str()))
            .filter(|event_id| refined.evidence_refs.contains(event_id))
            .collect();
        let evidence = if evidence.is_empty() {
            refined.evidence_refs.first().cloned().into_iter().collect()
        } else {
            evidence
        };
        refined
            .field_evidence
            .insert(ContractField::Summary, evidence);
    }
    let digest = blake3::hash(
        serde_json::to_string(&(
            &refined.objective,
            refined.status,
            &refined.summary,
            &refined.completed_steps,
            &refined.next_actions,
            &refined.blockers,
            &refined.constraints,
            &refined.completion_criteria,
            &refined.artifacts,
            &refined.verification,
        ))?
        .as_bytes(),
    );
    refined.material_content_hash = format!("b3:semantic-{}", digest.to_hex());
    refined.fact_set_digest = format!("{}:semantic-{}", previous.fact_set_digest, digest.to_hex());
    Ok(refined)
}

// Keep schemars in the dependency graph and make a machine-readable schema
// available to callers without coupling core's opaque identifiers to the
// schema derive implementation.
#[allow(dead_code)]
#[derive(JsonSchema)]
struct ProposedAssertionSchema {
    field: String,
    value: String,
    authority: String,
    evidence_refs: Vec<String>,
    confidence_basis_points: u16,
}

#[allow(dead_code)]
fn response_schema() -> schemars::Schema {
    schemars::schema_for!(ProposedAssertionSchema)
}
