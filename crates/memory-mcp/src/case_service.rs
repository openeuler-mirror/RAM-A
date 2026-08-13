use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use memory_cases::model::{
    CreateDatasetRequest, CreateDocumentFileRequest, SearchRequest as CaseLibrarySearchRequest,
    UpdateDocumentFileRequest,
};
use memory_cases::service::RagService;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    CaseDocumentDeleteRequest, CaseDocumentUpdateRequest, CaseDocumentUploadRequest,
    CaseLibraryConfig, CaseMutationConfirmationRequest, CaseSearchRequest, CaseServiceConfig,
    Principal,
};

const MAX_REFERENCE_CHARS: usize = 4_000;
const MAX_CONFIRMATION_PREVIEW_CHARS: usize = 320;
const CASE_CONFIRMATION_TTL_MS: u64 = 10 * 60 * 1_000;
const MAX_PENDING_CASE_CONFIRMATIONS: usize = 1_024;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CaseSearchResponse {
    pub library: String,
    pub references: Vec<CaseReference>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CaseReference {
    pub chunk_id: String,
    pub document_id: String,
    pub source_name: Option<String>,
    pub content: String,
    pub score: f32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CaseDocumentMutationResponse {
    pub library: String,
    pub document_id: String,
    pub task_id: String,
    pub ingestion_status: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CaseMutationProposalResponse {
    pub confirmation_token: String,
    pub operation: String,
    pub library: String,
    pub document_id: String,
    pub file_name: String,
    pub name: String,
    pub diagnosis_summary: String,
    pub content_sha256: String,
    pub content_preview: String,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CaseDeleteProposalResponse {
    pub confirmation_token: String,
    pub operation: String,
    pub library: String,
    pub document_id: String,
    pub name: String,
    pub deletion_reason: String,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CaseDocumentDeleteResponse {
    pub library: String,
    pub document_id: String,
    pub deleted: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaseServiceError {
    InvalidRequest,
    Forbidden,
    DocumentNotFound,
    DocumentConflict,
    ConfirmationRequired,
    ConfirmationInvalid,
    ConfirmationExpired,
    Unavailable,
    InvalidResponse,
    NotConfigured,
}

impl CaseServiceError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "CASE_INVALID_REQUEST",
            Self::Forbidden => "CASE_FORBIDDEN",
            Self::DocumentNotFound => "CASE_DOCUMENT_NOT_FOUND",
            Self::DocumentConflict => "CASE_DOCUMENT_CONFLICT",
            Self::ConfirmationRequired => "CASE_USER_CONFIRMATION_REQUIRED",
            Self::ConfirmationInvalid => "CASE_CONFIRMATION_INVALID",
            Self::ConfirmationExpired => "CASE_CONFIRMATION_EXPIRED",
            Self::Unavailable => "CASE_UNAVAILABLE",
            Self::InvalidResponse => "CASE_INVALID_RESPONSE",
            Self::NotConfigured => "CASE_NOT_CONFIGURED",
        }
    }

    pub fn retriable(self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

impl fmt::Display for CaseServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRequest => "case library request is invalid",
            Self::Forbidden => "case library access is forbidden",
            Self::DocumentNotFound => "case document was not found",
            Self::DocumentConflict => "case document already exists",
            Self::ConfirmationRequired => "explicit user confirmation is required",
            Self::ConfirmationInvalid => "case mutation confirmation is invalid or already used",
            Self::ConfirmationExpired => "case mutation confirmation has expired",
            Self::Unavailable => "case service is unavailable",
            Self::InvalidResponse => "case service returned an invalid response",
            Self::NotConfigured => "case service is not configured",
        })
    }
}

impl std::error::Error for CaseServiceError {}

#[async_trait]
pub trait CaseSearchProvider: Send + Sync {
    async fn search(
        &self,
        principal: &Principal,
        request: CaseSearchRequest,
    ) -> Result<CaseSearchResponse, CaseServiceError>;

    async fn prepare_upload_document(
        &self,
        _principal: &Principal,
        _request: CaseDocumentUploadRequest,
    ) -> Result<CaseMutationProposalResponse, CaseServiceError> {
        Err(CaseServiceError::NotConfigured)
    }

    async fn upload_document(
        &self,
        _principal: &Principal,
        _request: CaseMutationConfirmationRequest,
    ) -> Result<CaseDocumentMutationResponse, CaseServiceError> {
        Err(CaseServiceError::NotConfigured)
    }

    async fn prepare_update_document(
        &self,
        _principal: &Principal,
        _request: CaseDocumentUpdateRequest,
    ) -> Result<CaseMutationProposalResponse, CaseServiceError> {
        Err(CaseServiceError::NotConfigured)
    }

    async fn update_document(
        &self,
        _principal: &Principal,
        _request: CaseMutationConfirmationRequest,
    ) -> Result<CaseDocumentMutationResponse, CaseServiceError> {
        Err(CaseServiceError::NotConfigured)
    }

    async fn prepare_delete_document(
        &self,
        _principal: &Principal,
        _request: CaseDocumentDeleteRequest,
    ) -> Result<CaseDeleteProposalResponse, CaseServiceError> {
        Err(CaseServiceError::NotConfigured)
    }

    async fn delete_document(
        &self,
        _principal: &Principal,
        _request: CaseMutationConfirmationRequest,
    ) -> Result<CaseDocumentDeleteResponse, CaseServiceError> {
        Err(CaseServiceError::NotConfigured)
    }
}

pub type DynCaseSearchProvider = Arc<dyn CaseSearchProvider>;

#[derive(Clone)]
struct LibraryMapping {
    dataset_id: String,
    tenant_ids: HashSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ConfirmationOwner {
    tenant_id: String,
    user_id: String,
    agent_id: String,
}

impl From<&Principal> for ConfirmationOwner {
    fn from(principal: &Principal) -> Self {
        Self {
            tenant_id: principal.tenant_id.clone(),
            user_id: principal.user_id.clone(),
            agent_id: principal.agent_id.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaseMutationOperation {
    Upload,
    Update,
    Delete,
}

impl CaseMutationOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

#[derive(Clone, Debug)]
enum PendingCaseMutationRequest {
    Upload(CaseDocumentUploadRequest),
    Update(CaseDocumentUpdateRequest),
    Delete(CaseDocumentDeleteRequest),
}

impl PendingCaseMutationRequest {
    fn operation(&self) -> CaseMutationOperation {
        match self {
            Self::Upload(_) => CaseMutationOperation::Upload,
            Self::Update(_) => CaseMutationOperation::Update,
            Self::Delete(_) => CaseMutationOperation::Delete,
        }
    }
}

struct PendingCaseMutation {
    owner: ConfirmationOwner,
    expires_at_ms: u64,
    request: PendingCaseMutationRequest,
    executing: bool,
}

struct ConfirmationExecutionGuard {
    pending_confirmations: Arc<Mutex<HashMap<String, PendingCaseMutation>>>,
    confirmation_token: String,
}

impl Drop for ConfirmationExecutionGuard {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending_confirmations.lock() {
            if let Some(mutation) = pending.get_mut(&self.confirmation_token) {
                mutation.executing = false;
            }
        }
    }
}

#[derive(Clone)]
pub struct CaseServiceClient {
    http: reqwest::Client,
    base_url: url::Url,
    bearer_token: Arc<str>,
    max_response_bytes: usize,
    default_library: String,
    libraries: HashMap<String, LibraryMapping>,
}

#[derive(Clone)]
pub struct EmbeddedCaseSearchProvider {
    service: Arc<RagService>,
    default_library: String,
    libraries: HashMap<String, LibraryMapping>,
    pending_confirmations: Arc<Mutex<HashMap<String, PendingCaseMutation>>>,
}

impl EmbeddedCaseSearchProvider {
    pub fn new(
        service: Arc<RagService>,
        default_library: String,
        libraries: &[CaseLibraryConfig],
    ) -> Self {
        Self {
            service,
            default_library,
            libraries: libraries
                .iter()
                .map(|library| {
                    (
                        library.name.clone(),
                        LibraryMapping {
                            dataset_id: library.dataset_id.clone(),
                            tenant_ids: library.tenant_ids.iter().cloned().collect(),
                        },
                    )
                })
                .collect(),
            pending_confirmations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn authorized_library(
        &self,
        principal: &Principal,
        requested_library: Option<&str>,
    ) -> Result<(String, String), CaseServiceError> {
        let library_name = requested_library.unwrap_or(&self.default_library);
        let library = self
            .libraries
            .get(library_name)
            .filter(|library| library.tenant_ids.contains(&principal.tenant_id))
            .ok_or(CaseServiceError::Forbidden)?;
        Ok((library_name.to_owned(), library.dataset_id.clone()))
    }

    fn ensure_dataset(&self, library_name: &str, dataset_id: &str) -> Result<(), CaseServiceError> {
        let exists = self
            .service
            .list_datasets()
            .map_err(|_| CaseServiceError::Unavailable)?
            .datasets
            .iter()
            .any(|dataset| dataset.id == dataset_id);
        if exists {
            return Ok(());
        }
        if self
            .service
            .create_dataset(CreateDatasetRequest {
                id: Some(dataset_id.to_owned()),
                name: library_name.to_owned(),
                description: Some("RAM-A configured case library".to_owned()),
            })
            .is_ok()
        {
            return Ok(());
        }
        let created_concurrently = self
            .service
            .list_datasets()
            .map_err(|_| CaseServiceError::Unavailable)?
            .datasets
            .iter()
            .any(|dataset| dataset.id == dataset_id);
        if created_concurrently {
            Ok(())
        } else {
            Err(CaseServiceError::Unavailable)
        }
    }

    fn dataset_exists(&self, dataset_id: &str) -> Result<bool, CaseServiceError> {
        Ok(self
            .service
            .list_datasets()
            .map_err(|_| CaseServiceError::Unavailable)?
            .datasets
            .iter()
            .any(|dataset| dataset.id == dataset_id))
    }

    fn document_exists(
        &self,
        dataset_id: &str,
        document_id: &str,
    ) -> Result<bool, CaseServiceError> {
        if !self.dataset_exists(dataset_id)? {
            return Ok(false);
        }
        Ok(self
            .service
            .list_documents(dataset_id)
            .map_err(|_| CaseServiceError::Unavailable)?
            .documents
            .iter()
            .any(|document| document.id == document_id))
    }

    fn document(
        &self,
        dataset_id: &str,
        document_id: &str,
    ) -> Result<memory_cases::model::Document, CaseServiceError> {
        if !self.dataset_exists(dataset_id)? {
            return Err(CaseServiceError::DocumentNotFound);
        }
        self.service
            .list_documents(dataset_id)
            .map_err(|_| CaseServiceError::Unavailable)?
            .documents
            .into_iter()
            .find(|document| document.id == document_id)
            .ok_or(CaseServiceError::DocumentNotFound)
    }

    fn stage_mutation(
        &self,
        principal: &Principal,
        library: String,
        document_id: String,
        file_name: String,
        name: String,
        diagnosis_summary: String,
        content: &str,
        request: PendingCaseMutationRequest,
    ) -> Result<CaseMutationProposalResponse, CaseServiceError> {
        let content_sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        let (content_preview, _) = truncate_chars(content, MAX_CONFIRMATION_PREVIEW_CHARS);
        let operation = request.operation();
        let (confirmation_token, expires_at_ms) =
            self.store_pending_mutation(principal, request)?;

        Ok(CaseMutationProposalResponse {
            confirmation_token,
            operation: operation.as_str().to_owned(),
            library,
            document_id,
            file_name,
            name,
            diagnosis_summary,
            content_sha256,
            content_preview,
            expires_at_ms,
        })
    }

    fn store_pending_mutation(
        &self,
        principal: &Principal,
        request: PendingCaseMutationRequest,
    ) -> Result<(String, u64), CaseServiceError> {
        let now = current_time_ms();
        let expires_at_ms = now.saturating_add(CASE_CONFIRMATION_TTL_MS);
        let confirmation_token = Uuid::new_v4().to_string();
        let mut pending = self
            .pending_confirmations
            .lock()
            .map_err(|_| CaseServiceError::Unavailable)?;
        pending.retain(|_, mutation| mutation.expires_at_ms > now);
        if pending.len() >= MAX_PENDING_CASE_CONFIRMATIONS {
            return Err(CaseServiceError::Unavailable);
        }
        pending.insert(
            confirmation_token.clone(),
            PendingCaseMutation {
                owner: ConfirmationOwner::from(principal),
                expires_at_ms,
                request,
                executing: false,
            },
        );
        Ok((confirmation_token, expires_at_ms))
    }

    fn begin_confirmed_mutation(
        &self,
        principal: &Principal,
        confirmation: &CaseMutationConfirmationRequest,
        expected_operation: CaseMutationOperation,
    ) -> Result<PendingCaseMutationRequest, CaseServiceError> {
        if !confirmation.user_confirmed {
            return Err(CaseServiceError::ConfirmationRequired);
        }
        confirmation
            .validate()
            .map_err(|_| CaseServiceError::InvalidRequest)?;
        let now = current_time_ms();
        let owner = ConfirmationOwner::from(principal);
        let mut pending = self
            .pending_confirmations
            .lock()
            .map_err(|_| CaseServiceError::Unavailable)?;
        let Some(mutation) = pending.get_mut(&confirmation.confirmation_token) else {
            return Err(CaseServiceError::ConfirmationInvalid);
        };
        if mutation.expires_at_ms <= now {
            pending.remove(&confirmation.confirmation_token);
            return Err(CaseServiceError::ConfirmationExpired);
        }
        if mutation.owner != owner || mutation.request.operation() != expected_operation {
            return Err(CaseServiceError::ConfirmationInvalid);
        }
        if mutation.executing {
            return Err(CaseServiceError::Unavailable);
        }
        mutation.executing = true;
        Ok(mutation.request.clone())
    }

    fn finish_confirmed_mutation(
        &self,
        confirmation_token: &str,
        result: Result<(), CaseServiceError>,
    ) -> Result<(), CaseServiceError> {
        let mut pending = self
            .pending_confirmations
            .lock()
            .map_err(|_| CaseServiceError::Unavailable)?;
        if result == Err(CaseServiceError::Unavailable) {
            if let Some(mutation) = pending.get_mut(confirmation_token) {
                mutation.executing = false;
            }
        } else {
            pending.remove(confirmation_token);
        }
        Ok(())
    }

    fn confirmation_execution_guard(
        &self,
        confirmation_token: &str,
    ) -> ConfirmationExecutionGuard {
        ConfirmationExecutionGuard {
            pending_confirmations: self.pending_confirmations.clone(),
            confirmation_token: confirmation_token.to_owned(),
        }
    }

    async fn execute_upload(
        &self,
        principal: &Principal,
        operation_id: &str,
        request: CaseDocumentUploadRequest,
    ) -> Result<CaseDocumentMutationResponse, CaseServiceError> {
        request
            .validate()
            .map_err(|_| CaseServiceError::InvalidRequest)?;
        let (library_name, dataset_id) =
            self.authorized_library(principal, request.library.as_deref())?;
        self.ensure_dataset(&library_name, &dataset_id)?;
        if let Some(task) = self
            .service
            .get_task(operation_id)
            .map_err(|_| CaseServiceError::Unavailable)?
        {
            return Ok(CaseDocumentMutationResponse {
                library: library_name,
                document_id: task.document_id,
                task_id: task.id,
                ingestion_status: task.status,
            });
        }
        if let Some(document_id) = request.document_id.as_deref() {
            if self.document_exists(&dataset_id, document_id)? {
                return Err(CaseServiceError::DocumentConflict);
            }
        }

        let mime_type = request.mime_type().to_owned();
        let name = request
            .name
            .clone()
            .unwrap_or_else(|| request.file_name.clone());
        let response = self
            .service
            .create_document(
                &dataset_id,
                CreateDocumentFileRequest {
                    id: request.document_id.or_else(|| Some(operation_id.to_owned())),
                    task_id: Some(operation_id.to_owned()),
                    name,
                    file_name: request.file_name,
                    mime_type: Some(mime_type),
                    bytes: request.content.into_bytes(),
                },
            )
            .await
            .map_err(|_| CaseServiceError::Unavailable)?;

        Ok(CaseDocumentMutationResponse {
            library: library_name,
            document_id: response.document_id,
            task_id: response.task_id,
            ingestion_status: "pending".to_owned(),
        })
    }

    async fn execute_update(
        &self,
        principal: &Principal,
        operation_id: &str,
        request: CaseDocumentUpdateRequest,
    ) -> Result<CaseDocumentMutationResponse, CaseServiceError> {
        request
            .validate()
            .map_err(|_| CaseServiceError::InvalidRequest)?;
        let (library_name, dataset_id) =
            self.authorized_library(principal, request.library.as_deref())?;
        if let Some(task) = self
            .service
            .get_task(operation_id)
            .map_err(|_| CaseServiceError::Unavailable)?
        {
            return Ok(CaseDocumentMutationResponse {
                library: library_name,
                document_id: task.document_id,
                task_id: task.id,
                ingestion_status: task.status,
            });
        }
        if !self.document_exists(&dataset_id, &request.document_id)? {
            return Err(CaseServiceError::DocumentNotFound);
        }

        let mime_type = request.mime_type().to_owned();
        let response = self
            .service
            .update_document(
                &dataset_id,
                &request.document_id,
                UpdateDocumentFileRequest {
                    task_id: Some(operation_id.to_owned()),
                    name: request.name,
                    file_name: Some(request.file_name),
                    mime_type: Some(mime_type),
                    bytes: Some(request.content.into_bytes()),
                },
            )
            .await
            .map_err(|_| CaseServiceError::Unavailable)?;

        Ok(CaseDocumentMutationResponse {
            library: library_name,
            document_id: response.document_id,
            task_id: response.task_id,
            ingestion_status: "pending".to_owned(),
        })
    }

    async fn execute_delete(
        &self,
        principal: &Principal,
        request: CaseDocumentDeleteRequest,
    ) -> Result<CaseDocumentDeleteResponse, CaseServiceError> {
        request
            .validate()
            .map_err(|_| CaseServiceError::InvalidRequest)?;
        let (library_name, dataset_id) =
            self.authorized_library(principal, request.library.as_deref())?;
        if !self.document_exists(&dataset_id, &request.document_id)? {
            return Ok(CaseDocumentDeleteResponse {
                library: library_name,
                document_id: request.document_id,
                deleted: true,
            });
        }
        let response = self
            .service
            .delete_document(&dataset_id, &request.document_id)
            .await
            .map_err(|_| CaseServiceError::Unavailable)?;

        Ok(CaseDocumentDeleteResponse {
            library: library_name,
            document_id: response.document_id,
            deleted: response.deleted,
        })
    }
}

#[async_trait]
impl CaseSearchProvider for EmbeddedCaseSearchProvider {
    async fn search(
        &self,
        principal: &Principal,
        request: CaseSearchRequest,
    ) -> Result<CaseSearchResponse, CaseServiceError> {
        request
            .validate()
            .map_err(|_| CaseServiceError::InvalidRequest)?;
        let (library_name, dataset_id) =
            self.authorized_library(principal, request.library.as_deref())?;

        let response = self
            .service
            .search_dataset(
                &dataset_id,
                CaseLibrarySearchRequest {
                    query: request.query,
                    top_k: request.top_k,
                },
            )
            .await
            .map_err(|_| CaseServiceError::Unavailable)?;

        let mut truncated = response.chunks.len() > request.top_k;
        let mut references = Vec::with_capacity(response.chunks.len().min(request.top_k));
        for chunk in response.chunks.into_iter().take(request.top_k) {
            if chunk.dataset_id != dataset_id
                || chunk.chunk_id.trim().is_empty()
                || chunk.document_id.trim().is_empty()
                || chunk.content.trim().is_empty()
                || !chunk.score.is_finite()
            {
                return Err(CaseServiceError::InvalidResponse);
            }
            let (content, content_truncated) = truncate_chars(&chunk.content, MAX_REFERENCE_CHARS);
            truncated |= content_truncated;
            references.push(CaseReference {
                chunk_id: chunk.chunk_id,
                document_id: chunk.document_id,
                source_name: chunk.source_name,
                content,
                score: chunk.score,
            });
        }

        Ok(CaseSearchResponse {
            library: library_name,
            references,
            truncated,
        })
    }

    async fn prepare_upload_document(
        &self,
        principal: &Principal,
        mut request: CaseDocumentUploadRequest,
    ) -> Result<CaseMutationProposalResponse, CaseServiceError> {
        request
            .validate()
            .map_err(|_| CaseServiceError::InvalidRequest)?;
        let (library_name, dataset_id) =
            self.authorized_library(principal, request.library.as_deref())?;
        let document_id = request
            .document_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        if self.document_exists(&dataset_id, &document_id)? {
            return Err(CaseServiceError::DocumentConflict);
        }
        request.document_id = Some(document_id.clone());
        let file_name = request.file_name.clone();
        let name = request
            .name
            .clone()
            .unwrap_or_else(|| file_name.clone());
        let diagnosis_summary = request.diagnosis_summary.clone();
        let content = request.content.clone();
        self.stage_mutation(
            principal,
            library_name,
            document_id,
            file_name,
            name,
            diagnosis_summary,
            &content,
            PendingCaseMutationRequest::Upload(request),
        )
    }

    async fn upload_document(
        &self,
        principal: &Principal,
        confirmation: CaseMutationConfirmationRequest,
    ) -> Result<CaseDocumentMutationResponse, CaseServiceError> {
        let request = self.begin_confirmed_mutation(
            principal,
            &confirmation,
            CaseMutationOperation::Upload,
        )?;
        let _execution_guard =
            self.confirmation_execution_guard(&confirmation.confirmation_token);
        let result = match request {
            PendingCaseMutationRequest::Upload(request) => {
                self.execute_upload(principal, &confirmation.confirmation_token, request)
                    .await
            }
            PendingCaseMutationRequest::Update(_) | PendingCaseMutationRequest::Delete(_) => {
                Err(CaseServiceError::ConfirmationInvalid)
            }
        };
        self.finish_confirmed_mutation(
            &confirmation.confirmation_token,
            result.as_ref().map(|_| ()).map_err(|error| *error),
        )?;
        result
    }

    async fn prepare_update_document(
        &self,
        principal: &Principal,
        request: CaseDocumentUpdateRequest,
    ) -> Result<CaseMutationProposalResponse, CaseServiceError> {
        request
            .validate()
            .map_err(|_| CaseServiceError::InvalidRequest)?;
        let (library_name, dataset_id) =
            self.authorized_library(principal, request.library.as_deref())?;
        let existing_document = self.document(&dataset_id, &request.document_id)?;
        let document_id = request.document_id.clone();
        let file_name = request.file_name.clone();
        let name = request
            .name
            .clone()
            .unwrap_or(existing_document.name);
        let diagnosis_summary = request.diagnosis_summary.clone();
        let content = request.content.clone();
        self.stage_mutation(
            principal,
            library_name,
            document_id,
            file_name,
            name,
            diagnosis_summary,
            &content,
            PendingCaseMutationRequest::Update(request),
        )
    }

    async fn update_document(
        &self,
        principal: &Principal,
        confirmation: CaseMutationConfirmationRequest,
    ) -> Result<CaseDocumentMutationResponse, CaseServiceError> {
        let request = self.begin_confirmed_mutation(
            principal,
            &confirmation,
            CaseMutationOperation::Update,
        )?;
        let _execution_guard =
            self.confirmation_execution_guard(&confirmation.confirmation_token);
        let result = match request {
            PendingCaseMutationRequest::Update(request) => {
                self.execute_update(principal, &confirmation.confirmation_token, request)
                    .await
            }
            PendingCaseMutationRequest::Upload(_) | PendingCaseMutationRequest::Delete(_) => {
                Err(CaseServiceError::ConfirmationInvalid)
            }
        };
        self.finish_confirmed_mutation(
            &confirmation.confirmation_token,
            result.as_ref().map(|_| ()).map_err(|error| *error),
        )?;
        result
    }

    async fn prepare_delete_document(
        &self,
        principal: &Principal,
        request: CaseDocumentDeleteRequest,
    ) -> Result<CaseDeleteProposalResponse, CaseServiceError> {
        request
            .validate()
            .map_err(|_| CaseServiceError::InvalidRequest)?;
        let (library_name, dataset_id) =
            self.authorized_library(principal, request.library.as_deref())?;
        let document = self.document(&dataset_id, &request.document_id)?;
        let operation = CaseMutationOperation::Delete;
        let document_id = request.document_id.clone();
        let deletion_reason = request.deletion_reason.clone();
        let (confirmation_token, expires_at_ms) = self.store_pending_mutation(
            principal,
            PendingCaseMutationRequest::Delete(request),
        )?;

        Ok(CaseDeleteProposalResponse {
            confirmation_token,
            operation: operation.as_str().to_owned(),
            library: library_name,
            document_id,
            name: document.name,
            deletion_reason,
            expires_at_ms,
        })
    }

    async fn delete_document(
        &self,
        principal: &Principal,
        confirmation: CaseMutationConfirmationRequest,
    ) -> Result<CaseDocumentDeleteResponse, CaseServiceError> {
        let request = self.begin_confirmed_mutation(
            principal,
            &confirmation,
            CaseMutationOperation::Delete,
        )?;
        let _execution_guard =
            self.confirmation_execution_guard(&confirmation.confirmation_token);
        let result = match request {
            PendingCaseMutationRequest::Delete(request) => {
                self.execute_delete(principal, request).await
            }
            PendingCaseMutationRequest::Upload(_) | PendingCaseMutationRequest::Update(_) => {
                Err(CaseServiceError::ConfirmationInvalid)
            }
        };
        self.finish_confirmed_mutation(
            &confirmation.confirmation_token,
            result.as_ref().map(|_| ()).map_err(|error| *error),
        )?;
        result
    }
}

impl CaseServiceClient {
    pub fn from_config(config: &CaseServiceConfig) -> Result<Self> {
        config.validate()?;
        let token = std::env::var_os(&config.bearer_token_env).with_context(|| {
            format!(
                "case service credential environment variable `{}` is unavailable",
                config.bearer_token_env
            )
        })?;
        let token = token.into_string().map_err(|_| {
            anyhow::anyhow!(
                "case service credential environment variable `{}` is not valid Unicode",
                config.bearer_token_env
            )
        })?;
        if token.trim().is_empty() || token.trim() != token {
            bail!(
                "case service credential environment variable `{}` must be canonical and non-empty",
                config.bearer_token_env
            );
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()
            .context("failed to construct case service HTTP client")?;
        let libraries = config
            .libraries
            .iter()
            .map(|library| {
                (
                    library.name.clone(),
                    LibraryMapping {
                        dataset_id: library.dataset_id.clone(),
                        tenant_ids: library.tenant_ids.iter().cloned().collect(),
                    },
                )
            })
            .collect();
        Ok(Self {
            http,
            base_url: url::Url::parse(&config.base_url)
                .context("case service base URL is not valid")?,
            bearer_token: Arc::from(token),
            max_response_bytes: config.max_response_bytes,
            default_library: config.default_library.clone(),
            libraries,
        })
    }

    fn search_url(&self, dataset_id: &str) -> Result<url::Url, CaseServiceError> {
        let mut url = self.base_url.clone();
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| CaseServiceError::InvalidResponse)?;
        segments.pop_if_empty();
        segments.extend(["api", "v1", "datasets", dataset_id, "search"]);
        drop(segments);
        Ok(url)
    }
}

#[derive(Serialize)]
struct UpstreamSearchRequest<'a> {
    query: &'a str,
    top_k: usize,
}

#[derive(Deserialize)]
struct UpstreamSearchResponse {
    chunks: Vec<UpstreamChunk>,
}

#[derive(Deserialize)]
struct UpstreamChunk {
    chunk_id: String,
    dataset_id: String,
    document_id: String,
    source_name: Option<String>,
    content: String,
    score: f32,
}

#[async_trait]
impl CaseSearchProvider for CaseServiceClient {
    async fn search(
        &self,
        principal: &Principal,
        request: CaseSearchRequest,
    ) -> Result<CaseSearchResponse, CaseServiceError> {
        request
            .validate()
            .map_err(|_| CaseServiceError::InvalidRequest)?;
        let library_name = request.library.as_deref().unwrap_or(&self.default_library);
        let library = self
            .libraries
            .get(library_name)
            .filter(|library| library.tenant_ids.contains(&principal.tenant_id))
            .ok_or(CaseServiceError::Forbidden)?;
        let url = self.search_url(&library.dataset_id)?;
        let mut response = self
            .http
            .post(url)
            .bearer_auth(self.bearer_token.as_ref())
            .json(&UpstreamSearchRequest {
                query: &request.query,
                top_k: request.top_k,
            })
            .send()
            .await
            .map_err(|_| CaseServiceError::Unavailable)?;
        if !response.status().is_success() {
            return Err(CaseServiceError::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.max_response_bytes as u64)
        {
            return Err(CaseServiceError::InvalidResponse);
        }

        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| CaseServiceError::Unavailable)?
        {
            if bytes.len().saturating_add(chunk.len()) > self.max_response_bytes {
                return Err(CaseServiceError::InvalidResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        let upstream: UpstreamSearchResponse =
            serde_json::from_slice(&bytes).map_err(|_| CaseServiceError::InvalidResponse)?;

        let mut truncated = upstream.chunks.len() > request.top_k;
        let mut references = Vec::with_capacity(upstream.chunks.len().min(request.top_k));
        for chunk in upstream.chunks.into_iter().take(request.top_k) {
            if chunk.dataset_id != library.dataset_id
                || chunk.chunk_id.trim().is_empty()
                || chunk.document_id.trim().is_empty()
                || chunk.content.trim().is_empty()
                || !chunk.score.is_finite()
            {
                return Err(CaseServiceError::InvalidResponse);
            }
            let (content, content_truncated) = truncate_chars(&chunk.content, MAX_REFERENCE_CHARS);
            truncated |= content_truncated;
            references.push(CaseReference {
                chunk_id: chunk.chunk_id,
                document_id: chunk.document_id,
                source_name: chunk.source_name,
                content,
                score: chunk.score,
            });
        }
        Ok(CaseSearchResponse {
            library: library_name.to_owned(),
            references,
            truncated,
        })
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> (String, bool) {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        (truncated, true)
    } else {
        (truncated, false)
    }
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
pub struct DisabledCaseSearchProvider;

#[async_trait]
impl CaseSearchProvider for DisabledCaseSearchProvider {
    async fn search(
        &self,
        _principal: &Principal,
        _request: CaseSearchRequest,
    ) -> Result<CaseSearchResponse, CaseServiceError> {
        Err(CaseServiceError::NotConfigured)
    }
}

#[cfg(test)]
mod tests {
    use memory_cases::{build_service, CaseServiceOptions, EmbeddingProviderKind};
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn embedded_provider_uploads_updates_and_deletes_case_documents() {
        let temp = TempDir::new().unwrap();
        let service = build_service(&CaseServiceOptions {
            rag_store: temp.path().join("cases.sqlite"),
            memory_store: temp.path().join("case-index.sqlite"),
            embedding_provider: EmbeddingProviderKind::Hash,
            embedding_api_key_env: "UNUSED_CASE_EMBEDDING_KEY".to_owned(),
            embedding_base_url: "http://127.0.0.1:1/v1".to_owned(),
            embedding_model: "hash".to_owned(),
            embedding_dimensions: 32,
            chunk_size: 64,
            summary_llm_model: None,
            summary_llm_api_key_env: "UNUSED_CASE_SUMMARY_KEY".to_owned(),
            summary_llm_base_url: "http://127.0.0.1:1/v1".to_owned(),
            summary_llm_timeout_ms: 1_000,
        })
        .unwrap();
        let provider = EmbeddedCaseSearchProvider::new(
            service.clone(),
            "ops".to_owned(),
            &[CaseLibraryConfig {
                name: "ops".to_owned(),
                dataset_id: "ops-cases".to_owned(),
                tenant_ids: vec!["tenant-a".to_owned()],
            }],
        );
        let principal = Principal {
            tenant_id: "tenant-a".to_owned(),
            user_id: "admin".to_owned(),
            agent_id: "case-admin".to_owned(),
            permissions: vec!["cases:read".to_owned(), "cases:write".to_owned()],
        };

        let upload_proposal = provider
            .prepare_upload_document(
                &principal,
                CaseDocumentUploadRequest {
                    library: None,
                    document_id: Some("dns-case".to_owned()),
                    file_name: "dns-case.md".to_owned(),
                    name: Some("DNS incident".to_owned()),
                    diagnosis_summary: "The resolver cache contained a stale DNS record."
                        .to_owned(),
                    content: "# Old mitigation\n\nFlush olddnsneedle from the resolver cache."
                        .to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(upload_proposal.operation, "upload");
        assert!(service.list_datasets().unwrap().datasets.is_empty());

        let unconfirmed = provider
            .upload_document(
                &principal,
                CaseMutationConfirmationRequest {
                    confirmation_token: upload_proposal.confirmation_token.clone(),
                    user_confirmed: false,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(unconfirmed, CaseServiceError::ConfirmationRequired);
        assert!(service.list_datasets().unwrap().datasets.is_empty());

        let mut other_principal = principal.clone();
        other_principal.user_id = "other-admin".to_owned();
        let wrong_owner = provider
            .upload_document(
                &other_principal,
                CaseMutationConfirmationRequest {
                    confirmation_token: upload_proposal.confirmation_token.clone(),
                    user_confirmed: true,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(wrong_owner, CaseServiceError::ConfirmationInvalid);

        let wrong_operation = provider
            .update_document(
                &principal,
                CaseMutationConfirmationRequest {
                    confirmation_token: upload_proposal.confirmation_token.clone(),
                    user_confirmed: true,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(wrong_operation, CaseServiceError::ConfirmationInvalid);

        let uploaded = provider
            .upload_document(
                &principal,
                CaseMutationConfirmationRequest {
                    confirmation_token: upload_proposal.confirmation_token.clone(),
                    user_confirmed: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(uploaded.library, "ops");
        assert_eq!(uploaded.document_id, "dns-case");
        assert_eq!(uploaded.ingestion_status, "pending");

        let replay = provider
            .upload_document(
                &principal,
                CaseMutationConfirmationRequest {
                    confirmation_token: upload_proposal.confirmation_token,
                    user_confirmed: true,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(replay, CaseServiceError::ConfirmationInvalid);
        assert!(service.run_next_ingestion_task().await.unwrap());

        let old_search = provider
            .search(
                &principal,
                CaseSearchRequest {
                    query: "olddnsneedle".to_owned(),
                    library: None,
                    top_k: 5,
                },
            )
            .await
            .unwrap();
        assert_eq!(old_search.references[0].document_id, "dns-case");

        let update_proposal = provider
            .prepare_update_document(
                &principal,
                CaseDocumentUpdateRequest {
                    library: Some("ops".to_owned()),
                    document_id: "dns-case".to_owned(),
                    file_name: "dns-case.md".to_owned(),
                    name: None,
                    diagnosis_summary: "The previous mitigation was incomplete; the resolver must be restarted."
                        .to_owned(),
                    content: "# New mitigation\n\nRestart newdnsneedle after checking DNS."
                        .to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(update_proposal.operation, "update");
        assert_eq!(update_proposal.name, "DNS incident");

        let update_confirmation = CaseMutationConfirmationRequest {
            confirmation_token: update_proposal.confirmation_token.clone(),
            user_confirmed: true,
        };
        let begun = provider
            .begin_confirmed_mutation(
                &principal,
                &update_confirmation,
                CaseMutationOperation::Update,
            )
            .unwrap();
        assert!(matches!(begun, PendingCaseMutationRequest::Update(_)));
        let concurrent = provider
            .begin_confirmed_mutation(
                &principal,
                &update_confirmation,
                CaseMutationOperation::Update,
            )
            .unwrap_err();
        assert_eq!(concurrent, CaseServiceError::Unavailable);
        provider
            .finish_confirmed_mutation(
                &update_proposal.confirmation_token,
                Err(CaseServiceError::Unavailable),
            )
            .unwrap();

        let updated = provider
            .update_document(&principal, update_confirmation)
            .await
            .unwrap();
        assert_eq!(updated.document_id, "dns-case");
        assert_ne!(updated.task_id, uploaded.task_id);
        assert!(service.run_next_ingestion_task().await.unwrap());

        let new_search = provider
            .search(
                &principal,
                CaseSearchRequest {
                    query: "newdnsneedle".to_owned(),
                    library: None,
                    top_k: 5,
                },
            )
            .await
            .unwrap();
        assert_eq!(new_search.references[0].document_id, "dns-case");
        assert!(new_search.references[0].content.contains("newdnsneedle"));

        let duplicate = provider
            .prepare_upload_document(
                &principal,
                CaseDocumentUploadRequest {
                    library: None,
                    document_id: Some("dns-case".to_owned()),
                    file_name: "duplicate.md".to_owned(),
                    name: None,
                    diagnosis_summary: "A duplicate case was proposed.".to_owned(),
                    content: "Duplicate content".to_owned(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(duplicate, CaseServiceError::DocumentConflict);

        let delete_proposal = provider
            .prepare_delete_document(
                &principal,
                CaseDocumentDeleteRequest {
                    library: Some("ops".to_owned()),
                    document_id: "dns-case".to_owned(),
                    deletion_reason: "The case is obsolete and must no longer be suggested."
                        .to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(delete_proposal.operation, "delete");
        assert_eq!(delete_proposal.name, "DNS incident");

        let unconfirmed_delete = provider
            .delete_document(
                &principal,
                CaseMutationConfirmationRequest {
                    confirmation_token: delete_proposal.confirmation_token.clone(),
                    user_confirmed: false,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            unconfirmed_delete,
            CaseServiceError::ConfirmationRequired
        );
        assert_eq!(service.list_documents("ops-cases").unwrap().documents.len(), 1);

        let deleted = provider
            .delete_document(
                &principal,
                CaseMutationConfirmationRequest {
                    confirmation_token: delete_proposal.confirmation_token.clone(),
                    user_confirmed: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(deleted.library, "ops");
        assert_eq!(deleted.document_id, "dns-case");
        assert!(deleted.deleted);
        assert!(service.list_documents("ops-cases").unwrap().documents.is_empty());

        let delete_replay = provider
            .delete_document(
                &principal,
                CaseMutationConfirmationRequest {
                    confirmation_token: delete_proposal.confirmation_token,
                    user_confirmed: true,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(delete_replay, CaseServiceError::ConfirmationInvalid);

        let deleted_search = provider
            .search(
                &principal,
                CaseSearchRequest {
                    query: "newdnsneedle".to_owned(),
                    library: None,
                    top_k: 5,
                },
            )
            .await
            .unwrap();
        assert!(deleted_search.references.is_empty());

        let expiring_proposal = provider
            .prepare_upload_document(
                &principal,
                CaseDocumentUploadRequest {
                    library: None,
                    document_id: Some("expired-case".to_owned()),
                    file_name: "expired-case.md".to_owned(),
                    name: None,
                    diagnosis_summary: "This proposal is used to exercise expiry.".to_owned(),
                    content: "Expired proposal content".to_owned(),
                },
            )
            .await
            .unwrap();
        provider
            .pending_confirmations
            .lock()
            .unwrap()
            .get_mut(&expiring_proposal.confirmation_token)
            .unwrap()
            .expires_at_ms = 0;
        let expired = provider
            .upload_document(
                &principal,
                CaseMutationConfirmationRequest {
                    confirmation_token: expiring_proposal.confirmation_token,
                    user_confirmed: true,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(expired, CaseServiceError::ConfirmationExpired);
    }
}
