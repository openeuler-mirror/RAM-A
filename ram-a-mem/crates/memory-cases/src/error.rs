use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

/// HTTP error code returned for a malformed or invalid case API request.
pub const CASE_INVALID_REQUEST: &str = "CASE_INVALID_REQUEST";
/// HTTP error code returned when a requested case resource does not exist.
pub const CASE_NOT_FOUND: &str = "CASE_NOT_FOUND";
/// HTTP error code returned for an unexpected server-side failure.
pub const CASE_INTERNAL_ERROR: &str = "CASE_INTERNAL_ERROR";
/// HTTP error code returned when the case business database file is missing.
pub const CASE_BUSINESS_DATABASE_MISSING: &str = "CASE_BUSINESS_DATABASE_MISSING";
/// HTTP error code returned when the case index database file is missing.
pub const CASE_INDEX_DATABASE_MISSING: &str = "CASE_INDEX_DATABASE_MISSING";
/// HTTP error code returned when an administrator-requested index rebuild is already active.
pub const CASE_INDEX_REBUILD_IN_PROGRESS: &str = "CASE_INDEX_REBUILD_IN_PROGRESS";

#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    code: &'static str,
    message: String,
    retriable: bool,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    error: String,
    retriable: bool,
}

impl AppError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: CASE_INVALID_REQUEST,
            message: message.into(),
            retriable: false,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: CASE_NOT_FOUND,
            message: message.into(),
            retriable: false,
        }
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            message: message.into(),
            retriable: true,
        }
    }

    pub fn service_unavailable(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code,
            message: message.into(),
            retriable: true,
        }
    }

    pub fn internal(error: impl std::fmt::Display) -> Self {
        let detail = error.to_string();
        // The raw error may embed filesystem paths. Keep it in logs only and
        // return a client-safe message instead.
        tracing::error!(
            event = "ram_a.case.api.request.failed",
            operation = "memory_case",
            stage = "internal",
            error_code = CASE_INTERNAL_ERROR,
            error_detail = %detail
        );
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: CASE_INTERNAL_ERROR,
            message: "case internal error".to_string(),
            retriable: false,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                code: self.code,
                error: self.message,
                retriable: self.retriable,
            }),
        )
            .into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
