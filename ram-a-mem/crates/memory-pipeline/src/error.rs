use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ErrorOrigin {
    pub site: &'static str,
    pub file: &'static str,
    pub line: u32,
}

impl ErrorOrigin {
    #[track_caller]
    pub fn capture(site: &'static str) -> Self {
        let caller = std::panic::Location::caller();
        Self {
            site,
            file: caller.file(),
            line: caller.line(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PipelineStage {
    Normalize,
    Episode,
    Window,
    Extract,
    Validate,
    Ground,
    Aggregate,
}

impl PipelineStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normalize => "normalize",
            Self::Episode => "episode",
            Self::Window => "window",
            Self::Extract => "extract",
            Self::Validate => "validate",
            Self::Ground => "ground",
            Self::Aggregate => "aggregate",
        }
    }
}

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    Protocol(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{source}")]
    Located {
        origin: ErrorOrigin,
        #[source]
        source: Box<PipelineError>,
    },
    #[error("{source}")]
    Stage {
        stage: PipelineStage,
        origin: ErrorOrigin,
        #[source]
        source: Box<PipelineError>,
    },
}

impl PipelineError {
    #[track_caller]
    pub fn at_site(self, site: &'static str) -> Self {
        match self {
            Self::Located { .. } => self,
            source => Self::Located {
                origin: ErrorOrigin::capture(site),
                source: Box::new(source),
            },
        }
    }

    #[track_caller]
    pub fn at_stage(self, stage: PipelineStage) -> Self {
        match self {
            Self::Stage { .. } => self,
            source => Self::Stage {
                stage,
                origin: ErrorOrigin::capture(stage.site()),
                source: Box::new(source),
            },
        }
    }

    pub fn stage(&self) -> Option<PipelineStage> {
        match self {
            Self::Stage { stage, .. } => Some(*stage),
            Self::Located { source, .. } => source.stage(),
            _ => None,
        }
    }

    pub fn origin(&self) -> Option<ErrorOrigin> {
        match self {
            Self::Stage { origin, source, .. } => source.origin().or(Some(*origin)),
            Self::Located { origin, .. } => Some(*origin),
            _ => None,
        }
    }

    pub fn source_error_kind(&self) -> &'static str {
        match self.root() {
            Self::InvalidInput(_) => "invalid_input",
            Self::Json(_) => "invalid_json",
            Self::Io(_) => "io",
            Self::Protocol(message)
                if message.contains("reasoning content without final content") =>
            {
                "reasoning_only"
            }
            Self::Protocol(message) if message.contains("empty content") => "empty_content",
            Self::Protocol(message)
                if message.contains("valid JSON") || message.contains("invalid grounding JSON") =>
            {
                "invalid_json"
            }
            Self::Protocol(message)
                if message.contains("schema_version")
                    || message.contains("must be a list")
                    || message.contains("must be an object")
                    || message.contains("fields are invalid")
                    || message.contains("duplicate")
                    || message.contains("omitted memory") =>
            {
                "schema_invalid"
            }
            Self::Protocol(message) if message.contains("timed out") => "timeout",
            Self::Protocol(message) if message.contains("HTTP ") => "http_status",
            Self::Protocol(_) => "protocol",
            Self::Located { .. } | Self::Stage { .. } => {
                unreachable!("root removes diagnostic wrappers")
            }
        }
    }

    pub fn safe_summary(&self) -> &'static str {
        match self.source_error_kind() {
            "empty_content" => "model returned empty content",
            "reasoning_only" => "model returned reasoning without final content",
            "invalid_json" => "model returned invalid JSON",
            "schema_invalid" => "model response did not match the required schema",
            "timeout" => "model request timed out",
            "http_status" => "model provider returned an HTTP error",
            "invalid_input" => "pipeline input validation failed",
            "io" => "pipeline I/O operation failed",
            _ => "memory pipeline failed",
        }
    }

    /// Whether a caller retrying the same request could plausibly succeed.
    ///
    /// Invalid input, malformed JSON payloads, and schema violations are
    /// permanent for the same request body; transient provider, network, and
    /// I/O failures are not.
    pub fn is_retriable(&self) -> bool {
        !matches!(
            self.source_error_kind(),
            "invalid_input" | "invalid_json" | "schema_invalid"
        )
    }

    fn root(&self) -> &Self {
        match self {
            Self::Located { source, .. } | Self::Stage { source, .. } => source.root(),
            error => error,
        }
    }
}

impl PipelineStage {
    pub const fn site(self) -> &'static str {
        match self {
            Self::Normalize => "memory_pipeline.normalize",
            Self::Episode => "memory_pipeline.episode",
            Self::Window => "memory_pipeline.window",
            Self::Extract => "memory_pipeline.extract",
            Self::Validate => "memory_pipeline.validate",
            Self::Ground => "memory_pipeline.ground",
            Self::Aggregate => "memory_pipeline.aggregate",
        }
    }
}

pub type Result<T> = std::result::Result<T, PipelineError>;

#[cfg(test)]
mod tests {
    use super::{PipelineError, PipelineStage};

    #[test]
    fn stage_wrapping_preserves_the_first_error_origin() {
        let error = PipelineError::Protocol("extractor returned empty content".into())
            .at_site("memory_pipeline.extract.parse_response");
        let origin = error.origin().expect("located error origin");
        let wrapped = error.at_stage(PipelineStage::Extract);

        assert_eq!(wrapped.stage(), Some(PipelineStage::Extract));
        assert_eq!(wrapped.origin(), Some(origin));
        assert_eq!(
            wrapped.origin().unwrap().site,
            "memory_pipeline.extract.parse_response"
        );
        assert_eq!(wrapped.source_error_kind(), "empty_content");
    }
}
