use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{
    DisclosureClass, EvidenceId, RedactedText, RedactionPolicy,
    limits::{MAX_INLINE_BYTES, MAX_PREVIEW_BYTES},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LocalLocator {
    Evidence {
        evidence_id: EvidenceId,
    },
    SourceRange {
        source_id: String,
        generation: u64,
        byte_start: u64,
        byte_end: u64,
    },
}

#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct ContentRef {
    hash: String,
    byte_length: u64,
    media_type: String,
    local_locator: Option<LocalLocator>,
    redacted_excerpt: Option<String>,
    truncated: bool,
    disclosure_class: DisclosureClass,
}

impl fmt::Debug for ContentRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContentRef")
            .field("hash", &self.hash)
            .field("byte_length", &self.byte_length)
            .field("disclosure_class", &self.disclosure_class)
            .field(
                "redacted_excerpt_bytes",
                &self.redacted_excerpt.as_ref().map_or(0, String::len),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    #[error("content metadata exceeds its bound")]
    MetadataTooLarge,
    #[error("source range is invalid")]
    InvalidRange,
    #[error("redacted excerpt exceeds the preview bound")]
    PreviewTooLarge,
    #[error("serialized excerpt exceeds the inline-content bound")]
    ExcerptInputTooLarge,
    #[error("content truncation metadata is inconsistent")]
    InvalidTruncation,
    #[error("content excerpt disclosure class is forbidden")]
    ForbiddenDisclosure,
    #[error("content excerpt disclosure class does not match its reference")]
    DisclosureMismatch,
}

impl ContentRef {
    /// Builds a reference whose metadata and optional excerpt satisfy all
    /// content bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ContentError`] when metadata or a source range is invalid.
    pub fn bounded(
        hash: String,
        byte_length: u64,
        media_type: impl Into<String>,
        local_locator: Option<LocalLocator>,
        disclosure_class: DisclosureClass,
        redacted_excerpt: Option<RedactedText>,
    ) -> Result<Self, ContentError> {
        let media_type = media_type.into();
        let invalid_locator = matches!(
            &local_locator,
            Some(LocalLocator::SourceRange {
                source_id,
                byte_start,
                byte_end,
                ..
            }) if source_id.len() > 128 || byte_end < byte_start
        );
        if hash.len() > 128 || media_type.len() > 128 || invalid_locator {
            return Err(if invalid_locator {
                ContentError::InvalidRange
            } else {
                ContentError::MetadataTooLarge
            });
        }
        if let Some(excerpt) = &redacted_excerpt {
            if excerpt.disclosure_class() != disclosure_class {
                return Err(ContentError::DisclosureMismatch);
            }
            if !disclosure_class.is_transferable() {
                return Err(ContentError::ForbiddenDisclosure);
            }
        }
        let redacted_excerpt = redacted_excerpt.map(RedactedText::into_value);
        let content = Self {
            hash,
            byte_length,
            media_type,
            local_locator,
            redacted_excerpt,
            truncated: byte_length > MAX_INLINE_BYTES as u64,
            disclosure_class,
        };
        content.validate()?;
        Ok(content)
    }

    /// Revalidates the content reference before a store write.
    ///
    /// # Errors
    ///
    /// Returns [`ContentError`] when any content invariant is violated.
    pub fn validate(&self) -> Result<(), ContentError> {
        if self.hash.len() > 128 || self.media_type.len() > 128 {
            return Err(ContentError::MetadataTooLarge);
        }
        if self
            .redacted_excerpt
            .as_ref()
            .is_some_and(|excerpt| excerpt.len() > MAX_PREVIEW_BYTES)
        {
            return Err(ContentError::PreviewTooLarge);
        }
        if let Some(LocalLocator::SourceRange {
            source_id,
            byte_start,
            byte_end,
            ..
        }) = &self.local_locator
            && (source_id.len() > 128 || byte_end < byte_start)
        {
            return Err(ContentError::InvalidRange);
        }
        if self.truncated != (self.byte_length > MAX_INLINE_BYTES as u64) {
            return Err(ContentError::InvalidTruncation);
        }
        if self.redacted_excerpt.is_some() && !self.disclosure_class.is_transferable() {
            return Err(ContentError::ForbiddenDisclosure);
        }
        Ok(())
    }

    #[must_use]
    pub fn hash(&self) -> &str {
        &self.hash
    }

    #[must_use]
    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }

    #[must_use]
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    #[must_use]
    pub fn local_locator(&self) -> Option<&LocalLocator> {
        self.local_locator.as_ref()
    }

    #[must_use]
    pub fn redacted_excerpt(&self) -> Option<&str> {
        self.redacted_excerpt.as_deref()
    }

    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    #[must_use]
    pub fn disclosure_class(&self) -> DisclosureClass {
        self.disclosure_class
    }
}

#[derive(Deserialize)]
struct ContentRefWire {
    hash: String,
    byte_length: u64,
    media_type: String,
    local_locator: Option<LocalLocator>,
    redacted_excerpt: Option<String>,
    truncated: bool,
    disclosure_class: DisclosureClass,
}

impl<'de> Deserialize<'de> for ContentRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ContentRefWire::deserialize(deserializer)?;
        if wire
            .redacted_excerpt
            .as_ref()
            .is_some_and(|excerpt| excerpt.len() > MAX_INLINE_BYTES)
        {
            return Err(de::Error::custom(ContentError::ExcerptInputTooLarge));
        }
        if wire
            .redacted_excerpt
            .as_ref()
            .is_some_and(|excerpt| excerpt.len() > MAX_PREVIEW_BYTES)
        {
            return Err(de::Error::custom(ContentError::PreviewTooLarge));
        }
        let redacted_excerpt = wire
            .redacted_excerpt
            .map(|excerpt| {
                RedactionPolicy::new()
                    .and_then(|policy| policy.redact(&excerpt, None, wire.disclosure_class))
            })
            .transpose()
            .map_err(de::Error::custom)?;
        let content = Self::bounded(
            wire.hash,
            wire.byte_length,
            wire.media_type,
            wire.local_locator,
            wire.disclosure_class,
            redacted_excerpt,
        )
        .map_err(de::Error::custom)?;
        if content.truncated != wire.truncated {
            return Err(de::Error::custom(ContentError::InvalidTruncation));
        }
        Ok(content)
    }
}
