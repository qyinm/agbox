//! Authority and evidence filtering for optional semantic refinement.
//!
//! This module is deliberately independent from the HTTP client.  A remote
//! model can suggest text, but only immutable local evidence can grant that
//! text authority in a contract field.

use std::collections::BTreeMap;

use agbox_core::{Authority, DisclosureClass, EvidenceId, ProjectId};

use crate::semantic::{ProposedAssertion, ProposedAssertions, SemanticError};

/// Bounded, already-redacted evidence presented to an extractor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorityEvidence {
    pub evidence_id: EvidenceId,
    pub project_id: Option<ProjectId>,
    pub authority: Authority,
    pub disclosure_class: DisclosureClass,
    pub excerpt: String,
}

impl AuthorityEvidence {
    #[must_use]
    pub fn new(
        evidence_id: EvidenceId,
        authority: Authority,
        disclosure_class: DisclosureClass,
        excerpt: impl Into<String>,
    ) -> Self {
        Self {
            evidence_id,
            project_id: None,
            authority,
            disclosure_class,
            excerpt: excerpt.into(),
        }
    }

    #[must_use]
    pub fn in_project(
        project_id: ProjectId,
        evidence_id: EvidenceId,
        authority: Authority,
        disclosure_class: DisclosureClass,
        excerpt: impl Into<String>,
    ) -> Self {
        Self {
            evidence_id,
            project_id: Some(project_id),
            authority,
            disclosure_class,
            excerpt: excerpt.into(),
        }
    }
}

/// Immutable authority policy used before a model proposal can affect a
/// refined contract.
#[derive(Clone, Debug, Default)]
pub struct SemanticPolicy {
    evidence: BTreeMap<EvidenceId, AuthorityEvidence>,
    project_id: Option<ProjectId>,
}

impl SemanticPolicy {
    /// Builds an evidence policy without a project scope.
    ///
    /// Use [`Self::for_project`] at the coordinator boundary; an unscoped
    /// policy is useful for pure filtering tests but is intentionally rejected
    /// by semantic publication.
    #[must_use]
    pub fn with_evidence<I>(evidence: I) -> Self
    where
        I: IntoIterator<Item = AuthorityEvidence>,
    {
        Self {
            evidence: evidence
                .into_iter()
                .map(|item| (item.evidence_id.clone(), item))
                .collect(),
            project_id: None,
        }
    }

    #[must_use]
    pub fn for_project<I>(project_id: ProjectId, evidence: I) -> Self
    where
        I: IntoIterator<Item = AuthorityEvidence>,
    {
        let mut policy = Self::with_evidence(evidence);
        policy.project_id = Some(project_id);
        policy
    }

    #[must_use]
    pub fn project_matches(&self, project_id: &ProjectId) -> bool {
        self.project_id.as_ref() == Some(project_id)
    }

    /// Filters proposals into authority-safe assertions.
    ///
    /// Unknown fields, unsupported authority transitions, missing evidence,
    /// oversized values, and low-trust instruction proposals are omitted. A
    /// malformed response is therefore safe to treat as an empty refinement.
    ///
    /// # Errors
    ///
    /// Returns a bounded policy error when the response cardinality or value
    /// limits are exceeded. Individual untrusted assertions are filtered.
    pub fn validate(
        &self,
        proposals: ProposedAssertions,
    ) -> Result<ProposedAssertions, SemanticError> {
        proposals.validate_shape()?;

        let mut selected: BTreeMap<String, ProposedAssertion> = BTreeMap::new();
        for proposal in proposals.assertions {
            if proposal.value.len() > crate::semantic::MAX_ASSERTION_VALUE_BYTES
                || proposal.confidence_basis_points > 10_000
            {
                return Err(SemanticError::InvalidProposal);
            }
            let field = normalize_field(&proposal.field);
            if !is_supported_field(&field) {
                continue;
            }
            let effective = self.effective_authority(&proposal);
            let Some(authority) = effective else {
                continue;
            };
            if !allowed_for_field(&field, authority) {
                continue;
            }
            if is_instruction_field(&field) && !self.matches_human_intent(&proposal) {
                continue;
            }
            let mut filtered = proposal;
            filtered.field.clone_from(&field);
            filtered.authority = authority;
            let replace = selected
                .get(&field)
                .is_none_or(|existing| authority > existing.authority);
            if replace {
                selected.insert(field, filtered);
            }
        }
        Ok(ProposedAssertions {
            assertions: selected.into_values().collect(),
        })
    }

    fn effective_authority(&self, proposal: &ProposedAssertion) -> Option<Authority> {
        if proposal.evidence_refs.is_empty() {
            return None;
        }
        proposal
            .evidence_refs
            .iter()
            .map(|id| {
                self.evidence.get(id).and_then(|evidence| {
                    if self
                        .project_id
                        .as_ref()
                        .is_some_and(|project| evidence.project_id.as_ref() != Some(project))
                        || !disclosure_matches_authority(
                            evidence.authority,
                            evidence.disclosure_class,
                        )
                    {
                        None
                    } else {
                        Some(evidence.authority)
                    }
                })
            })
            .collect::<Option<Vec<_>>>()
            .and_then(|authorities| authorities.into_iter().max())
    }

    fn matches_human_intent(&self, proposal: &ProposedAssertion) -> bool {
        let proposed = normalize_text(&proposal.value);
        !proposed.is_empty()
            && proposal.evidence_refs.iter().any(|id| {
                self.evidence.get(id).is_some_and(|evidence| {
                    evidence.authority == Authority::HumanIntent
                        && normalize_text(&evidence.excerpt) == proposed
                        && evidence.disclosure_class == DisclosureClass::HumanIntent
                })
            })
    }
}

fn disclosure_matches_authority(authority: Authority, disclosure: DisclosureClass) -> bool {
    matches!(
        (authority, disclosure),
        (Authority::HumanIntent, DisclosureClass::HumanIntent)
            | (Authority::ToolResult, DisclosureClass::ToolResult)
            | (Authority::ObservedState, DisclosureClass::ObservedState)
            | (Authority::AgentStatement, DisclosureClass::AgentStatement)
            | (Authority::ModelInference, DisclosureClass::DerivedText)
    )
}

fn normalize_field(field: &str) -> String {
    field.trim().to_ascii_lowercase().replace('-', "_")
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_supported_field(field: &str) -> bool {
    matches!(
        field,
        "objective"
            | "constraint"
            | "constraints"
            | "completion_criteria"
            | "completion_criterion"
            | "verification"
            | "summary"
            | "next_action"
            | "next_actions"
            | "blocker"
            | "blockers"
    )
}

fn is_instruction_field(field: &str) -> bool {
    matches!(
        field,
        "objective"
            | "constraint"
            | "constraints"
            | "completion_criteria"
            | "completion_criterion"
            | "next_action"
            | "next_actions"
    )
}

fn allowed_for_field(field: &str, authority: Authority) -> bool {
    match field {
        "objective"
        | "constraint"
        | "constraints"
        | "completion_criteria"
        | "completion_criterion" => authority == Authority::HumanIntent,
        "verification" => matches!(authority, Authority::ToolResult | Authority::ObservedState),
        // Agent/model text may only be a summary, never an instruction.
        "summary" => true,
        "next_action" | "next_actions" | "blocker" | "blockers" => {
            authority == Authority::HumanIntent
        }
        _ => false,
    }
}
