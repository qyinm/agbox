//! Optional, bounded semantic refinement.
//!
//! Refinement is disabled unless a caller explicitly supplies a validated
//! loopback URL.  This module never executes a proposed action and never
//! sends source paths, reasoning, or unrestricted tool output over HTTP.

use std::time::Duration;
use std::{collections::BTreeSet, net::IpAddr};

use agbox_core::{
    Authority, ContractId, DisclosureClass, EvidenceId, PrivacyLabel, ProjectId, RedactionPolicy,
    WorkContractRevision, WorkStatus,
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
    #[error("semantic response contains no authority-safe assertions")]
    NoAssertions,
    #[error("semantic response produces no material contract change")]
    NoOp,
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
    pub project_id: ProjectId,
    pub evidence_id: EvidenceId,
    pub authority: Authority,
    pub disclosure_class: DisclosureClass,
    pub privacy: PrivacyLabel,
    pub text: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BoundedEvidence {
    pub project_id: ProjectId,
    pub evidence_id: EvidenceId,
    pub authority: Authority,
    pub disclosure_class: DisclosureClass,
    pub privacy: PrivacyLabel,
    pub excerpt: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct BoundedArtifact {
    pub project_id: ProjectId,
    pub artifact_id: String,
    pub state: String,
    pub privacy: PrivacyLabel,
    pub disclosure_class: DisclosureClass,
}

#[derive(Clone, Debug)]
pub struct ExtractionInput {
    pub previous_contract: WorkContractRevision,
    pub new_facts: Vec<BoundedFact>,
    pub evidence_excerpts: Vec<BoundedEvidence>,
    pub artifact_state: Vec<BoundedArtifact>,
}

/// The serializable projection deliberately omits source-run IDs,
/// evidence-object IDs, and disclosure metadata. They are local provenance,
/// not semantic input, and must never be accepted as extractor egress.
#[derive(Serialize)]
struct ExtractionInputWire<'a> {
    previous_contract: EgressContract<'a>,
    new_facts: &'a [BoundedFact],
    evidence_excerpts: &'a [BoundedEvidence],
    artifact_state: &'a [BoundedArtifact],
}

#[derive(Serialize)]
struct EgressContract<'a> {
    contract_id: &'a ContractId,
    work_id: &'a agbox_core::WorkId,
    revision: u64,
    project_id: &'a ProjectId,
    objective: Option<&'a str>,
    status: WorkStatus,
    summary: &'a str,
    completed_steps: &'a [String],
    next_actions: &'a [String],
    blockers: &'a [String],
    constraints: &'a [String],
    completion_criteria: &'a [String],
    artifacts: &'a [String],
    verification: &'a [String],
    confidence_basis_points: u16,
    created_at: time::OffsetDateTime,
}

impl<'a> From<&'a WorkContractRevision> for EgressContract<'a> {
    fn from(contract: &'a WorkContractRevision) -> Self {
        Self {
            contract_id: contract.contract_id(),
            work_id: contract.work_id(),
            revision: contract.revision(),
            project_id: contract.project_id(),
            objective: contract.objective(),
            status: contract.status(),
            summary: contract.summary(),
            completed_steps: contract.completed_steps(),
            next_actions: contract.next_actions(),
            blockers: contract.blockers(),
            constraints: contract.constraints(),
            completion_criteria: contract.completion_criteria(),
            artifacts: contract.artifacts(),
            verification: contract.verification(),
            confidence_basis_points: contract.confidence_basis_points(),
            created_at: contract.created_at(),
        }
    }
}

impl Serialize for ExtractionInput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.validate_egress().map_err(serde::ser::Error::custom)?;
        ExtractionInputWire {
            previous_contract: EgressContract::from(&self.previous_contract),
            new_facts: &self.new_facts,
            evidence_excerpts: &self.evidence_excerpts,
            artifact_state: &self.artifact_state,
        }
        .serialize(serializer)
    }
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
        if contract_contains_absolute_path(&previous_contract) {
            return Err(SemanticError::InvalidProposal);
        }
        let project_id = previous_contract.project_id().clone();
        if new_facts.iter().any(|fact| fact.project_id != project_id)
            || evidence_excerpts
                .iter()
                .any(|evidence| evidence.project_id != project_id)
            || artifact_state
                .iter()
                .any(|artifact| artifact.project_id != project_id)
        {
            return Err(SemanticError::InvalidProposal);
        }
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
        artifact_state.retain(|artifact| {
            artifact.privacy != PrivacyLabel::RestrictedLocal
                && artifact.disclosure_class.is_transferable()
                && !contains_absolute_path(&artifact.artifact_id)
                && !contains_absolute_path(&artifact.state)
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
        input.validate_egress()?;
        Ok(input)
    }

    /// Verifies that a caller-constructed input remains bound to one project.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidProposal`] when any input projection or
    /// its previous contract belongs to another project or work item.
    pub fn validate_project(
        &self,
        project_id: &ProjectId,
        work_id: &agbox_core::WorkId,
    ) -> Result<(), SemanticError> {
        self.validate_egress()?;
        if self.previous_contract.project_id() != project_id
            || self.previous_contract.work_id() != work_id
            || self
                .new_facts
                .iter()
                .any(|fact| &fact.project_id != project_id)
            || self
                .evidence_excerpts
                .iter()
                .any(|evidence| &evidence.project_id != project_id)
            || self
                .artifact_state
                .iter()
                .any(|artifact| &artifact.project_id != project_id)
        {
            return Err(SemanticError::InvalidProposal);
        }
        Ok(())
    }

    fn validate_egress(&self) -> Result<(), SemanticError> {
        if contract_contains_absolute_path(&self.previous_contract)
            || self.new_facts.iter().any(|fact| {
                fact.privacy == PrivacyLabel::RestrictedLocal
                    || !fact.disclosure_class.is_transferable()
                    || fact.text.as_deref().is_some_and(contains_absolute_path)
            })
            || self.evidence_excerpts.iter().any(|evidence| {
                evidence.privacy == PrivacyLabel::RestrictedLocal
                    || !evidence.disclosure_class.is_transferable()
                    || contains_absolute_path(&evidence.excerpt)
            })
            || self.artifact_state.iter().any(|artifact| {
                artifact.privacy == PrivacyLabel::RestrictedLocal
                    || !artifact.disclosure_class.is_transferable()
                    || contains_absolute_path(&artifact.artifact_id)
                    || contains_absolute_path(&artifact.state)
            })
        {
            return Err(SemanticError::InvalidProposal);
        }
        Ok(())
    }
}

fn contract_contains_absolute_path(contract: &WorkContractRevision) -> bool {
    contract.objective().is_some_and(contains_absolute_path)
        || contains_absolute_path(contract.summary())
        || contract
            .completed_steps()
            .iter()
            .chain(contract.next_actions())
            .chain(contract.blockers())
            .chain(contract.constraints())
            .chain(contract.completion_criteria())
            .chain(contract.artifacts())
            .chain(contract.verification())
            .any(|value| contains_absolute_path(value))
}

fn contains_absolute_path(value: &str) -> bool {
    let characters = value.chars().collect::<Vec<_>>();
    let windows_drive_path = characters.windows(3).any(|characters| {
        characters[0].is_ascii_alphabetic()
            && matches!(characters[1], ':' | '：')
            && matches!(characters[2], '\\' | '/')
    });
    let rooted_path = characters.iter().enumerate().any(|(index, character)| {
        matches!(character, '\\' | '/') && (index == 0 || is_path_delimiter(characters[index - 1]))
    });
    windows_drive_path || rooted_path
}

fn is_path_delimiter(character: char) -> bool {
    character.is_whitespace() || (character != '_' && !character.is_alphanumeric())
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
        input.validate_egress()?;
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

/// Refines a contract while using the policy's immutable evidence-to-event
/// mapping for provenance.
///
/// # Errors
///
/// Returns an error for empty, unproven, unsupported, or non-material
/// proposals, as well as bounded redaction or serialization failures.
pub fn refine_provisional_contract_at_with_policy(
    previous: &ProvisionalContract,
    proposals: &ProposedAssertions,
    extractor_version: impl Into<String>,
    observed_at: time::OffsetDateTime,
    policy: &SemanticPolicy,
) -> Result<ProvisionalContract, SemanticError> {
    proposals.validate_shape()?;
    if proposals.assertions.is_empty() {
        return Err(SemanticError::NoAssertions);
    }
    let mut refined = previous.clone();
    let redaction = RedactionPolicy::new()?;
    for assertion in &proposals.assertions {
        if assertion.value.trim().is_empty() {
            return Err(SemanticError::InvalidProposal);
        }
        let field = canonical_field(&assertion.field).ok_or(SemanticError::InvalidProposal)?;
        let redacted = redaction.redact(&assertion.value, None, DisclosureClass::DerivedText)?;
        let events = policy.event_ids_for(assertion);
        if events.is_empty() {
            return Err(SemanticError::InvalidProposal);
        }
        let mut all_evidence = refined
            .evidence_refs
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        all_evidence.extend(events.iter().cloned());
        if all_evidence.len() > agbox_core::limits::MAX_CONTRACT_EVIDENCE_REFS {
            return Err(SemanticError::InvalidProposal);
        }
        refined.evidence_refs = all_evidence.into_iter().collect();
        refined.field_evidence.insert(field, events);
        match field {
            ContractField::Objective => refined.objective = Some(redacted.value().to_owned()),
            ContractField::Summary => redacted.value().clone_into(&mut refined.summary),
            ContractField::Constraints => refined.constraints = vec![redacted.value().to_owned()],
            ContractField::CompletionCriteria => {
                refined.completion_criteria = vec![redacted.value().to_owned()];
            }
            ContractField::NextActions => refined.next_actions = vec![redacted.value().to_owned()],
            ContractField::Blockers => refined.blockers = vec![redacted.value().to_owned()],
            ContractField::Verification => refined.verification = vec![redacted.value().to_owned()],
            ContractField::Status | ContractField::CompletedSteps | ContractField::Artifacts => {
                return Err(SemanticError::InvalidProposal);
            }
        }
    }
    if material_content(&refined) == material_content(previous) {
        return Err(SemanticError::NoOp);
    }
    refined.revision = previous
        .revision
        .checked_add(1)
        .ok_or(SemanticError::InvalidProposal)?;
    refined.extractor_version = extractor_version.into();
    refined.created_at = observed_at;
    let digest = blake3::hash(serde_json::to_string(&material_content(&refined))?.as_bytes());
    refined.material_content_hash = format!("b3:semantic-{}", digest.to_hex());
    refined.fact_set_digest = format!("{}:semantic-{}", previous.fact_set_digest, digest.to_hex());
    Ok(refined)
}

fn canonical_field(field: &str) -> Option<ContractField> {
    match field.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "objective" => Some(ContractField::Objective),
        "constraint" | "constraints" => Some(ContractField::Constraints),
        "completion_criteria" | "completion_criterion" => Some(ContractField::CompletionCriteria),
        "summary" => Some(ContractField::Summary),
        "next_action" | "next_actions" => Some(ContractField::NextActions),
        "blocker" | "blockers" => Some(ContractField::Blockers),
        "verification" => Some(ContractField::Verification),
        _ => None,
    }
}

#[derive(PartialEq, Serialize)]
struct MaterialContent<'a> {
    objective: &'a Option<String>,
    status: agbox_core::WorkStatus,
    summary: &'a String,
    completed_steps: &'a Vec<String>,
    next_actions: &'a Vec<String>,
    blockers: &'a Vec<String>,
    constraints: &'a Vec<String>,
    completion_criteria: &'a Vec<String>,
    artifacts: &'a Vec<String>,
    verification: &'a Vec<String>,
}

fn material_content(contract: &ProvisionalContract) -> MaterialContent<'_> {
    MaterialContent {
        objective: &contract.objective,
        status: contract.status,
        summary: &contract.summary,
        completed_steps: &contract.completed_steps,
        next_actions: &contract.next_actions,
        blockers: &contract.blockers,
        constraints: &contract.constraints,
        completion_criteria: &contract.completion_criteria,
        artifacts: &contract.artifacts,
        verification: &contract.verification,
    }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use agbox_core::{ContractId, EvidenceId, WorkContractRevisionDraft, WorkStatus};
    use time::macros::datetime;

    use super::*;

    fn previous_contract(project_id: ProjectId, summary: &str) -> WorkContractRevision {
        let redaction = RedactionPolicy::new().unwrap();
        WorkContractRevision::new(WorkContractRevisionDraft {
            contract_id: ContractId::parse_wire("contract-egress").unwrap(),
            work_id: agbox_core::WorkId::parse_wire("work-egress").unwrap(),
            revision: 1,
            project_id,
            objective: None,
            status: WorkStatus::Active,
            summary: redaction
                .redact(summary, None, DisclosureClass::DerivedText)
                .unwrap(),
            completed_steps: Vec::new(),
            next_actions: Vec::new(),
            blockers: Vec::new(),
            constraints: Vec::new(),
            completion_criteria: Vec::new(),
            artifacts: Vec::new(),
            verification: Vec::new(),
            source_runs: Vec::new(),
            evidence_refs: vec![EvidenceId::parse_wire("evidence-egress").unwrap()],
            confidence_basis_points: 9_000,
            created_at: datetime!(2026-07-19 12:00 UTC),
            extractor_version: "deterministic-v1".into(),
            disclosure_class: DisclosureClass::DerivedText,
        })
        .unwrap()
    }

    #[test]
    fn bounded_input_filters_embedded_and_artifact_absolute_paths() {
        let project_id = ProjectId::parse_wire("project-egress").unwrap();
        let input = ExtractionInput::bounded(
            previous_contract(project_id.clone(), "safe summary"),
            vec![BoundedFact {
                project_id: project_id.clone(),
                evidence_id: EvidenceId::parse_wire("fact-egress").unwrap(),
                authority: Authority::AgentStatement,
                disclosure_class: DisclosureClass::AgentStatement,
                privacy: PrivacyLabel::PrivateLocal,
                text: Some("changed,/Users/alice/private.rs".into()),
            }],
            vec![BoundedEvidence {
                project_id: project_id.clone(),
                evidence_id: EvidenceId::parse_wire("evidence-egress").unwrap(),
                authority: Authority::AgentStatement,
                disclosure_class: DisclosureClass::AgentStatement,
                privacy: PrivacyLabel::PrivateLocal,
                excerpt: "diagnostic C:\\Users\\alice\\secret".into(),
            }],
            vec![
                BoundedArtifact {
                    project_id,
                    artifact_id: "artifact-1".into(),
                    state: "modified /private/tmp/secret.txt".into(),
                    privacy: PrivacyLabel::PrivateLocal,
                    disclosure_class: DisclosureClass::AgentStatement,
                },
                BoundedArtifact {
                    project_id: ProjectId::parse_wire("project-egress").unwrap(),
                    artifact_id: "artifact-unc".into(),
                    state: "modified \\\\server\\share\\secret.txt".into(),
                    privacy: PrivacyLabel::PrivateLocal,
                    disclosure_class: DisclosureClass::AgentStatement,
                },
            ],
        )
        .unwrap();
        assert!(input.new_facts.is_empty());
        assert!(input.evidence_excerpts.is_empty());
        assert!(input.artifact_state.is_empty());
        let encoded = serde_json::to_string(&input).unwrap();
        assert!(!encoded.contains("source_runs"));
        assert!(!encoded.contains("evidence_refs"));
        assert!(!encoded.contains("disclosure_class"));
        assert!(!encoded.contains("extractor_version"));
    }

    #[test]
    fn absolute_path_detector_covers_previous_contract_text_forms() {
        for path in [
            "/Users/alice/private.rs",
            "C:\\Users\\alice\\private.rs",
            "\\\\server\\share\\private.rs",
            "\\Users\\alice\\private.rs",
            "“/Users/alice/private.rs",
            "경로：\\server\\share\\private.rs",
        ] {
            assert!(contains_absolute_path(path));
        }
    }
}
