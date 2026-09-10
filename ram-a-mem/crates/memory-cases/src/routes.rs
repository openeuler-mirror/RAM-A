use std::sync::Arc;

use axum::{
    extract::{Multipart, Path, Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use subtle::ConstantTimeEq;

use crate::error::{AppError, AppResult};
use crate::model::{
    ChatCompletionRequest, CreateDatasetRequest, CreateDocumentFileRequest, SearchRequest,
    UpdateDocumentFileRequest,
};
use crate::service::{observable_error_kind, observable_error_summary, RagService};

#[derive(Clone)]
struct ApiAuthState {
    bearer_token: Arc<str>,
}

impl ApiAuthState {
    fn new(token: impl Into<String>) -> Self {
        Self {
            bearer_token: Arc::from(token.into()),
        }
    }
}

/// Builds the authenticated case-management API for embedding in an owning
/// HTTP service such as `ram-a-mem`.
pub fn create_api_router(service: Arc<RagService>, bearer_token: String) -> Router {
    let api = Router::new()
        .route("/api/v1/datasets", post(create_dataset).get(list_datasets))
        .route(
            "/api/v1/datasets/:dataset_id/documents",
            post(create_document).get(list_documents),
        )
        .route(
            "/api/v1/datasets/:dataset_id/documents/:document_id",
            put(update_document).delete(delete_document),
        )
        .route("/api/v1/tasks/:task_id", get(get_task))
        .route(
            "/api/v1/datasets/:dataset_id/documents/:document_id/chunks",
            get(list_chunks),
        )
        .route("/api/v1/datasets/:dataset_id/search", post(search_dataset))
        .route("/api/v1/chat/completions", post(chat_completion))
        .route("/api/v1/index/rebuilds", post(start_index_rebuild))
        .route(
            "/api/v1/index/rebuilds/:operation_id",
            get(get_index_rebuild_status),
        );
    Router::new()
        .merge(protect_api_routes(api, ApiAuthState::new(bearer_token)))
        .with_state(service)
}

fn protect_api_routes<S>(router: Router<S>, auth: ApiAuthState) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router.route_layer(middleware::from_fn_with_state(auth, authorize_api))
}

async fn authorize_api(State(auth): State<ApiAuthState>, request: Request, next: Next) -> Response {
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| bool::from(token.as_bytes().ct_eq(auth.bearer_token.as_bytes())));
    if authorized {
        return next.run(request).await;
    }

    let mut response = (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

async fn create_dataset(
    State(service): State<Arc<RagService>>,
    Json(request): Json<CreateDatasetRequest>,
) -> AppResult<Json<crate::model::Dataset>> {
    service
        .create_dataset(request)
        .map(Json)
        .map_err(map_service_error)
}

async fn list_datasets(
    State(service): State<Arc<RagService>>,
) -> AppResult<Json<crate::model::ListDatasetsResponse>> {
    service
        .list_datasets()
        .map(Json)
        .map_err(map_service_error)
}

async fn create_document(
    State(service): State<Arc<RagService>>,
    Path(dataset_id): Path<String>,
    multipart: Multipart,
) -> AppResult<Json<crate::model::CreateDocumentResponse>> {
    let request = parse_document_upload(multipart).await?;
    service
        .create_document(&dataset_id, request)
        .await
        .map(Json)
        .map_err(map_service_error)
}

async fn list_documents(
    State(service): State<Arc<RagService>>,
    Path(dataset_id): Path<String>,
) -> AppResult<Json<crate::model::ListDocumentsResponse>> {
    service
        .list_documents(&dataset_id)
        .map(Json)
        .map_err(map_service_error)
}

async fn update_document(
    State(service): State<Arc<RagService>>,
    Path((dataset_id, document_id)): Path<(String, String)>,
    multipart: Multipart,
) -> AppResult<Json<crate::model::UpdateDocumentResponse>> {
    let request = parse_document_update(multipart).await?;
    service
        .update_document(&dataset_id, &document_id, request)
        .await
        .map(Json)
        .map_err(map_service_error)
}

async fn delete_document(
    State(service): State<Arc<RagService>>,
    Path((dataset_id, document_id)): Path<(String, String)>,
) -> AppResult<Json<crate::model::DeleteDocumentResponse>> {
    service
        .delete_document(&dataset_id, &document_id)
        .await
        .map(Json)
        .map_err(map_service_error)
}

async fn get_task(
    State(service): State<Arc<RagService>>,
    Path(task_id): Path<String>,
) -> AppResult<Json<crate::model::IngestionTask>> {
    let task = service.get_task(&task_id).map_err(map_service_error)?;
    task.map(Json)
        .ok_or_else(|| AppError::not_found("task not found"))
}

async fn list_chunks(
    State(service): State<Arc<RagService>>,
    Path((dataset_id, document_id)): Path<(String, String)>,
) -> AppResult<Json<crate::model::ListChunksResponse>> {
    service
        .list_chunks(&dataset_id, &document_id)
        .map(Json)
        .map_err(map_service_error)
}

async fn search_dataset(
    State(service): State<Arc<RagService>>,
    Path(dataset_id): Path<String>,
    Json(request): Json<SearchRequest>,
) -> AppResult<Json<crate::model::SearchResponse>> {
    service
        .search_dataset(&dataset_id, request)
        .await
        .map(Json)
        .map_err(map_service_error)
}

async fn chat_completion(
    State(service): State<Arc<RagService>>,
    Json(request): Json<ChatCompletionRequest>,
) -> AppResult<Json<crate::model::ChatCompletionResponse>> {
    service
        .chat_completion(request)
        .await
        .map(Json)
        .map_err(map_service_error)
}

async fn start_index_rebuild(
    State(service): State<Arc<RagService>>,
) -> AppResult<(StatusCode, Json<crate::model::IndexRebuildStatus>)> {
    service
        .start_index_rebuild()
        .map(|status| (StatusCode::ACCEPTED, Json(status)))
        .map_err(map_service_error)
}

async fn get_index_rebuild_status(
    State(service): State<Arc<RagService>>,
    Path(operation_id): Path<String>,
) -> AppResult<Json<crate::model::IndexRebuildStatus>> {
    service
        .get_index_rebuild_status(&operation_id)
        .map_err(map_service_error)?
        .map(Json)
        .ok_or_else(|| AppError::not_found("case index rebuild operation not found"))
}

fn map_service_error(error: anyhow::Error) -> AppError {
    let message = error.to_string();
    if crate::is_business_database_missing(&error) {
        let code = crate::error::CASE_BUSINESS_DATABASE_MISSING;
        log_database_missing_api_error(&error, code);
        // Client-safe text: the raw message embeds the configured database path.
        AppError::service_unavailable(code, "case business database is missing")
    } else if crate::is_case_index_database_missing(&error) {
        let code = crate::error::CASE_INDEX_DATABASE_MISSING;
        log_database_missing_api_error(&error, code);
        AppError::service_unavailable(
            code,
            "case index database is missing; an administrator must start an index rebuild",
        )
    } else if crate::is_case_index_rebuild_in_progress(&error) {
        AppError::conflict(crate::error::CASE_INDEX_REBUILD_IN_PROGRESS, message)
    } else if message.contains("not found") {
        AppError::not_found(message)
    } else if message.contains("must")
        || message.contains("requires")
        || message.contains("required")
    {
        AppError::bad_request(message)
    } else {
        AppError::internal(message)
    }
}

fn log_database_missing_api_error(error: &anyhow::Error, error_code: &'static str) {
    tracing::error!(
        event = "ram_a.case.api.request.failed",
        operation = "memory_case",
        stage = "storage",
        error_code,
        error_kind = observable_error_kind(error),
        error = observable_error_summary(error),
        // Full detail, including the configured database path, stays in logs only.
        error_detail = %error,
        retriable = true,
        http_status = StatusCode::SERVICE_UNAVAILABLE.as_u16()
    );
}

async fn parse_document_upload(mut multipart: Multipart) -> AppResult<CreateDocumentFileRequest> {
    let mut id = None;
    let mut task_id = None;
    let mut name = None;
    let mut file_name = None;
    let mut mime_type = None;
    let mut bytes = None;

    while let Some(field) = multipart.next_field().await.map_err(AppError::internal)? {
        match field.name().unwrap_or_default() {
            "id" => id = Some(field.text().await.map_err(AppError::internal)?),
            "task_id" => task_id = Some(field.text().await.map_err(AppError::internal)?),
            "name" => name = Some(field.text().await.map_err(AppError::internal)?),
            "file" => {
                file_name = field.file_name().map(str::to_string);
                mime_type = field.content_type().map(str::to_string);
                bytes = Some(field.bytes().await.map_err(AppError::internal)?.to_vec());
            }
            _ => {}
        }
    }

    let file_name = file_name.ok_or_else(|| AppError::bad_request("file is required"))?;
    Ok(CreateDocumentFileRequest {
        id,
        task_id,
        name: name.unwrap_or_else(|| file_name.clone()),
        file_name,
        mime_type,
        bytes: bytes.ok_or_else(|| AppError::bad_request("file is required"))?,
    })
}

async fn parse_document_update(mut multipart: Multipart) -> AppResult<UpdateDocumentFileRequest> {
    let mut task_id = None;
    let mut name = None;
    let mut file_name = None;
    let mut mime_type = None;
    let mut bytes = None;

    while let Some(field) = multipart.next_field().await.map_err(AppError::internal)? {
        match field.name().unwrap_or_default() {
            "task_id" => task_id = Some(field.text().await.map_err(AppError::internal)?),
            "name" => name = Some(field.text().await.map_err(AppError::internal)?),
            "file" => {
                file_name = field.file_name().map(str::to_string);
                mime_type = field.content_type().map(str::to_string);
                bytes = Some(field.bytes().await.map_err(AppError::internal)?.to_vec());
            }
            _ => {}
        }
    }

    Ok(UpdateDocumentFileRequest {
        task_id,
        name,
        file_name,
        mime_type,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use serde_json::{json, Value};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::{create_api_router, protect_api_routes, ApiAuthState};
    use crate::model::{CreateDatasetRequest, CreateDocumentFileRequest};
    use crate::service::{RagConfig, RagService};
    use crate::token_counter::TestTokenCounter;
    use crate::{build_service, CaseServiceOptions, EmbeddingProviderKind};
    use memory_core::{EmbeddingProvider, MemoryManager, SqliteMemoryStore};

    fn remove_sqlite_database(path: &Path) {
        let _ = std::fs::remove_file(path);
        for suffix in ["wal", "shm"] {
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(format!("-{suffix}"));
            let _ = std::fs::remove_file(sidecar);
        }
    }

    fn test_service(temp: &TempDir) -> Arc<crate::service::RagService> {
        build_service(&CaseServiceOptions {
            rag_store: temp.path().join("cases.sqlite"),
            memory_store: temp.path().join("case-index.sqlite"),
            embedding_provider: EmbeddingProviderKind::Hash,
            embedding_api_key_env: "UNUSED_CASE_EMBEDDING_KEY".to_string(),
            embedding_base_url: "http://127.0.0.1:1/v1".to_string(),
            embedding_model: "hash".to_string(),
            embedding_dimensions: 32,
            chunk_size: 32,
            summary_llm_model: None,
            summary_llm_api_key_env: "UNUSED_CASE_SUMMARY_KEY".to_string(),
            summary_llm_base_url: "http://127.0.0.1:1/v1".to_string(),
            summary_llm_timeout_ms: 1_000,
        })
        .expect("build case service")
    }

    async fn response_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        serde_json::from_slice(&bytes).expect("decode response JSON")
    }

    #[tokio::test]
    async fn api_routes_require_the_exact_bearer_token() {
        let protected = protect_api_routes(
            Router::new().route("/protected", get(|| async { StatusCode::NO_CONTENT })),
            ApiAuthState::new("internal-secret"),
        );

        for authorization in [
            None,
            Some("internal-secret"),
            Some("Bearer wrong-secret"),
            Some("bearer internal-secret"),
        ] {
            let mut request = Request::builder().uri("/protected");
            if let Some(authorization) = authorization {
                request = request.header(header::AUTHORIZATION, authorization);
            }
            let response = protected
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
                "Bearer"
            );
        }

        let authorized = protected
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header(header::AUTHORIZATION, "Bearer internal-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn admin_can_start_and_observe_an_asynchronous_index_rebuild() {
        let temp = TempDir::new().unwrap();
        let service = test_service(&temp);
        service
            .create_dataset(CreateDatasetRequest {
                id: Some("ops".to_string()),
                name: "Operations".to_string(),
                description: None,
            })
            .unwrap();
        service
            .create_document(
                "ops",
                CreateDocumentFileRequest {
                    id: Some("dns".to_string()),
                    task_id: Some("dns-task".to_string()),
                    name: "dns.md".to_string(),
                    file_name: "dns.md".to_string(),
                    mime_type: Some("text/markdown".to_string()),
                    bytes: b"# DNS failure\n\nFlush the resolver cache.".to_vec(),
                },
            )
            .await
            .unwrap();
        assert!(service.run_next_ingestion_task().await.unwrap());

        let index_path = temp.path().join("case-index.sqlite");
        remove_sqlite_database(&index_path);
        let app = create_api_router(service, "admin-secret".to_string());

        let search_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/datasets/ops/search")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"query": "DNS", "top_k": 5}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(search_response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let error = response_json(search_response).await;
        assert_eq!(error["code"], crate::error::CASE_INDEX_DATABASE_MISSING);
        assert_eq!(error["retriable"], true);
        assert!(!index_path.exists());

        let start_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/index/rebuilds")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start_response.status(), StatusCode::ACCEPTED);
        let started = response_json(start_response).await;
        let operation_id = started["operation_id"].as_str().unwrap().to_string();

        let completed = wait_for_finished_rebuild(&app, &operation_id).await;
        assert_eq!(completed["state"], "completed", "{completed}");
        assert_eq!(completed["document_count"], 1);
        assert!(index_path.exists());

        remove_sqlite_database(&temp.path().join("cases.sqlite"));
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/datasets/ops/search")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"query": "DNS", "top_k": 5}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let error = response_json(response).await;
        assert_eq!(error["code"], crate::error::CASE_BUSINESS_DATABASE_MISSING);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/datasets")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let error = response_json(response).await;
        assert_eq!(error["code"], crate::error::CASE_BUSINESS_DATABASE_MISSING);
        assert_eq!(error["retriable"], true);
    }

    /// Embedding provider that blocks long enough for route-level tests to
    /// observe the active rebuild window deterministically.
    struct SlowEmbeddingProvider {
        delay: Duration,
    }

    #[async_trait::async_trait]
    impl EmbeddingProvider for SlowEmbeddingProvider {
        fn dimensions(&self) -> usize {
            32
        }

        async fn embed(&self, texts: &[String]) -> memory_core::MemoryResult<Vec<Vec<f32>>> {
            tokio::time::sleep(self.delay).await;
            Ok(texts
                .iter()
                .map(|_| vec![0.0f32; self.dimensions()])
                .collect())
        }
    }

    async fn slow_rebuild_service(temp: &TempDir) -> Arc<RagService> {
        let repo = Arc::new(crate::repo::RagRepository::new(
            temp.path().join("cases.sqlite"),
        ));
        repo.initialize().expect("initialize case repository");
        let store = Arc::new(SqliteMemoryStore::new_existing(
            temp.path().join("case-index.sqlite"),
        ));
        store
            .initialize_blocking()
            .expect("initialize case index store");
        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(SlowEmbeddingProvider {
            delay: Duration::from_millis(250),
        });
        let memory = Arc::new(MemoryManager::new(store.clone(), embedder.clone()));
        let service = Arc::new(RagService::new(
            repo,
            memory,
            embedder,
            store,
            RagConfig {
                file_root: temp.path().join("files"),
                chunk_size: 32,
                token_counter: Arc::new(TestTokenCounter),
                summary_llm: None,
            },
        ));
        service
            .create_dataset(CreateDatasetRequest {
                id: Some("dataset-1".to_string()),
                name: "Dataset".to_string(),
                description: None,
            })
            .expect("create dataset");
        service
            .create_document(
                "dataset-1",
                CreateDocumentFileRequest {
                    id: Some("document-1".to_string()),
                    task_id: Some("task-1".to_string()),
                    name: "case.txt".to_string(),
                    file_name: "case.txt".to_string(),
                    mime_type: Some("text/plain".to_string()),
                    bytes: b"slow rebuild case content for the route test".to_vec(),
                },
            )
            .await
            .expect("create document");
        assert!(service
            .run_next_ingestion_task()
            .await
            .expect("run ingestion task"));
        service
    }

    async fn wait_for_finished_rebuild(app: &Router, operation_id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(format!("/api/v1/index/rebuilds/{operation_id}"))
                            .header(header::AUTHORIZATION, "Bearer admin-secret")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let status = response_json(response).await;
                if matches!(status["state"].as_str(), Some("completed" | "failed")) {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("rebuild should finish")
    }

    #[tokio::test]
    async fn rebuild_conflict_and_operation_history_are_visible_over_http() {
        let temp = TempDir::new().unwrap();
        let service = slow_rebuild_service(&temp).await;
        let app = create_api_router(service, "admin-secret".to_string());

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/index/rebuilds")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let first_body = response_json(first).await;
        let first_id = first_body["operation_id"]
            .as_str()
            .expect("operation id")
            .to_string();

        // While the rebuild is active a second start is rejected with 409.
        let conflict = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/index/rebuilds")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        let conflict_body = response_json(conflict).await;
        assert_eq!(
            conflict_body["code"],
            crate::error::CASE_INDEX_REBUILD_IN_PROGRESS
        );

        let finished = wait_for_finished_rebuild(&app, &first_id).await;
        assert_eq!(finished["state"], "completed");

        // Starting a retry must keep the finished operation queryable.
        let retry = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/index/rebuilds")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(retry.status(), StatusCode::ACCEPTED);

        let first_after_retry = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/index/rebuilds/{first_id}"))
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first_after_retry.status(), StatusCode::OK);
        let first_after_retry = response_json(first_after_retry).await;
        assert_eq!(first_after_retry["operation_id"], first_id);
        assert_eq!(first_after_retry["state"], "completed");

        // Unknown ids still return 404.
        let unknown = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/index/rebuilds/unknown-operation-id")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        let unknown_body = response_json(unknown).await;
        assert_eq!(unknown_body["code"], crate::error::CASE_NOT_FOUND);
    }
}
