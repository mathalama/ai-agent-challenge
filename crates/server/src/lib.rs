use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use agent::{Input, ProviderFactory, Session};
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Multipart, Path as RoutePath, Query, Request, State},
    http::{HeaderMap, StatusCode, header, uri::Authority},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use llm::settings::{SettingsPatch, SettingsView};
use review::{JsonRenderer, MarkdownRenderer, Report, ReportRenderer};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tower_http::services::{ServeDir, ServeFile};
use uuid::Uuid;

mod events;
#[cfg(test)]
mod tests;
use events::Events;

const MAX_UPLOAD: usize = 20 * 1024 * 1024;
const MAX_SESSIONS: usize = 64;
const SESSION_IDLE: Duration = Duration::from_secs(3600);

#[derive(Clone)]
struct App {
    sessions: Arc<Mutex<HashMap<Uuid, SessionEntry>>>,
    provider: Arc<ProviderFactory>,
    config_path: Option<PathBuf>,
}

struct SessionEntry {
    last_used: Instant,
    session: Arc<Chat>,
}

impl SessionEntry {
    fn is_expired(&self) -> bool {
        self.last_used.elapsed() >= SESSION_IDLE && Arc::strong_count(&self.session) == 1
    }
}

struct Chat {
    session: Arc<AsyncMutex<Session>>,
    events: Arc<Events>,
}

type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug)]
struct ApiError(StatusCode, &'static str);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

pub async fn serve(
    address: SocketAddr,
    provider: ProviderFactory,
    config_path: Option<PathBuf>,
    web_dir: PathBuf,
    trust_local_host: bool,
) -> agent::Result<()> {
    let listener = tokio::net::TcpListener::bind(address).await?;
    let router = router(
        provider,
        config_path,
        &web_dir,
        address.ip().is_loopback(),
        trust_local_host,
    );
    println!("Сервер доступен на http://{}", listener.local_addr()?);
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn router(
    provider: ProviderFactory,
    config_path: Option<PathBuf>,
    web_dir: &Path,
    loopback: bool,
    trust_local_host: bool,
) -> Router {
    let app = App {
        sessions: Arc::default(),
        provider: Arc::new(provider),
        config_path,
    };
    let settings = Router::new()
        .route("/settings", get(get_settings).put(put_settings))
        .route("/settings/test", post(test_settings))
        .route("/settings/openrouter/models", get(openrouter_models))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .route_layer(middleware::from_fn_with_state(
            trust_local_host,
            local_settings,
        ));
    let api = Router::new()
        .merge(settings)
        .route("/schema", get(|| async { Json(Report::json_schema()) }))
        .route("/sessions", post(create_session))
        .route("/sessions/{id}", get(session_info))
        .route(
            "/sessions/{id}/messages",
            post(message).layer(DefaultBodyLimit::max(128 * 1024)),
        )
        .route(
            "/sessions/{id}/document",
            post(upload).layer(DefaultBodyLimit::max(MAX_UPLOAD + 64 * 1024)),
        )
        .route("/sessions/{id}/events", get(session_events))
        .route("/sessions/{id}/report", get(report))
        .fallback(|| async {
            ApiError(StatusCode::NOT_FOUND, "Маршрут API не найден")
        })
        .layer(middleware::from_fn_with_state(loopback, same_origin))
        .with_state(app);
    Router::new()
        .nest("/api", api)
        .fallback_service(
            ServeDir::new(web_dir).fallback(ServeFile::new(web_dir.join("index.html"))),
        )
        .layer(middleware::from_fn(static_cache))
}

/// Сборка кладёт в `assets/` файлы с хешем в имени, их можно кэшировать навсегда.
/// Остальные файлы, включая `index.html`, браузер сверяет с сервером при каждой загрузке,
/// иначе после пересборки он подгружает старый бандл.
async fn static_cache(request: Request, next: Next) -> Response {
    let immutable = request.uri().path().starts_with("/assets/");
    let mut response = next.run(request).await;
    if !response.headers().contains_key(header::CACHE_CONTROL) {
        let value = if immutable {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static(value),
        );
    }
    response
}

async fn local_settings(
    State(trust_local_host): State<bool>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let loopback = trust_local_host
        || request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .is_some_and(|ConnectInfo(peer)| peer.ip().to_canonical().is_loopback());
    let matching_origin = headers.get(header::ORIGIN).is_none_or(|origin| {
        origin
            .to_str()
            .ok()
            .and_then(|origin| origin.parse::<axum::http::Uri>().ok())
            .filter(|uri| matches!(uri.scheme_str(), Some("http" | "https")))
            .and_then(|uri| uri.authority().cloned())
            .is_some_and(|origin| {
                headers.get(header::HOST).is_some_and(|host| {
                    host.to_str()
                        .is_ok_and(|value| value.eq_ignore_ascii_case(origin.as_str()))
                })
            })
    });
    if !(loopback && local_host(headers) && matching_origin) {
        return ApiError(
            StatusCode::FORBIDDEN,
            "Настройки доступны только на localhost",
        )
        .into_response();
    }
    next.run(request).await
}

async fn get_settings(State(app): State<App>) -> ApiResult<Json<SettingsView>> {
    app.provider.view().map(Json).map_err(settings_error)
}

async fn put_settings(
    State(app): State<App>,
    Json(patch): Json<SettingsPatch>,
) -> ApiResult<Json<SettingsView>> {
    let path = app.config_path.as_deref().ok_or(ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "Каталог профиля недоступен",
    ))?;
    app.provider
        .update(path, patch)
        .map(Json)
        .map_err(settings_error)
}

async fn test_settings(
    State(app): State<App>,
    Json(patch): Json<SettingsPatch>,
) -> ApiResult<Json<Value>> {
    let config = app.provider.preview(patch).map_err(settings_error)?;
    match llm::test_settings(&config).await {
        Ok(message) => Ok(Json(json!({"message": message}))),
        Err(llm::LlmError::Settings(message)) => Err(ApiError(StatusCode::BAD_REQUEST, message)),
        Err(_) => Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "Не удалось проверить соединение. Проверьте ключ, модель и адрес.",
        )),
    }
}

fn settings_error(error: Box<dyn std::error::Error + Send + Sync>) -> ApiError {
    if let Some(llm::LlmError::Settings(message)) = error.downcast_ref() {
        ApiError(StatusCode::BAD_REQUEST, message)
    } else {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Не удалось обработать настройки",
        )
    }
}

#[derive(Deserialize)]
struct CatalogQuery {
    #[serde(default = "free_models_default")]
    free_only: bool,
}

fn free_models_default() -> bool {
    true
}

async fn openrouter_models(
    Query(query): Query<CatalogQuery>,
) -> ApiResult<Json<Vec<llm::openrouter::RouterModel>>> {
    llm::openrouter::models(query.free_only)
        .await
        .map(Json)
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "Каталог OpenRouter недоступен"))
}

async fn same_origin(State(loopback): State<bool>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    if (loopback && !local_host(headers))
        || headers
            .get("sec-fetch-site")
            .is_some_and(|site| site.as_bytes() == b"cross-site")
    {
        return ApiError(StatusCode::FORBIDDEN, "Запрос с другого сайта").into_response();
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

fn local_host(headers: &HeaderMap) -> bool {
    headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .and_then(|host| host.parse::<Authority>().ok())
        .is_some_and(|host| {
            let host = host.host().trim_matches(['[', ']']);
            host.eq_ignore_ascii_case("localhost")
                || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        })
}

async fn create_session(State(app): State<App>) -> ApiResult<(StatusCode, Json<Value>)> {
    let mut sessions = app.sessions.lock().expect("блокировка сессий");
    sessions.retain(|_, entry| {
        if entry.is_expired() {
            entry.session.events.expire();
            false
        } else {
            true
        }
    });
    if sessions.len() >= MAX_SESSIONS {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Открыто слишком много сессий",
        ));
    }
    let id = Uuid::new_v4();
    let factory = Arc::clone(&app.provider);
    let session = Session::new(move |trace, reasoning| factory.build(trace, reasoning));
    sessions.insert(
        id,
        SessionEntry {
            last_used: Instant::now(),
            session: Arc::new(Chat {
                session: Arc::new(AsyncMutex::new(session)),
                events: Arc::default(),
            }),
        },
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id.to_string(),
            "greeting": Session::greeting(),
            "provider": app.provider.resolved().ok().map(|config| config.name()),
        })),
    ))
}

/// Данные для повторного подключения к сессии после перезагрузки страницы.
/// `last_event_id` отделяет уже прошедшие события журнала от новых.
async fn session_info(
    State(app): State<App>,
    RoutePath(id): RoutePath<String>,
) -> ApiResult<Json<Value>> {
    let chat = app.chat(&id)?;
    Ok(Json(json!({
        "id": id,
        "greeting": Session::greeting(),
        "provider": app.provider.resolved().ok().map(|config| config.name()),
        "last_event_id": chat.events.last_id(),
    })))
}

impl App {
    fn chat(&self, id: &str) -> ApiResult<Arc<Chat>> {
        let missing = || ApiError(StatusCode::NOT_FOUND, "Сессия не найдена");
        let id = Uuid::parse_str(id).map_err(|_| missing())?;
        let mut sessions = self.sessions.lock().expect("блокировка сессий");
        if sessions.get(&id).is_some_and(SessionEntry::is_expired) {
            if let Some(entry) = sessions.remove(&id) {
                entry.session.events.expire();
            }
            return Err(missing());
        }
        let entry = sessions.get_mut(&id).ok_or_else(missing)?;
        entry.last_used = Instant::now();
        Ok(Arc::clone(&entry.session))
    }
}

impl Chat {
    fn begin(self: &Arc<Self>) -> ApiResult<OwnedMutexGuard<Session>> {
        Arc::clone(&self.session).try_lock_owned().map_err(|_| {
            ApiError(
                StatusCode::CONFLICT,
                "Дождитесь окончания предыдущей команды",
            )
        })
    }

    async fn run(self: Arc<Self>, mut session: OwnedMutexGuard<Session>, input: Input<'_>) {
        let events = Arc::clone(&self.events);
        session.handle(input, move |event| events.push(event)).await;
        drop(session);
        self.events.done();
    }
}

#[derive(Deserialize)]
struct Message {
    text: String,
}

async fn message(
    State(app): State<App>,
    RoutePath(id): RoutePath<String>,
    Json(message): Json<Message>,
) -> ApiResult<StatusCode> {
    let chat = app.chat(&id)?;
    if message.text.trim().is_empty() || message.text.len() > 16_384 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Введите сообщение длиной до 16 КБ",
        ));
    }
    let session = chat.begin()?;
    tokio::spawn(async move { chat.run(session, Input::Text(&message.text)).await });
    Ok(StatusCode::ACCEPTED)
}

async fn upload(
    State(app): State<App>,
    RoutePath(id): RoutePath<String>,
    mut multipart: Multipart,
) -> ApiResult<StatusCode> {
    let chat = app.chat(&id)?;
    let invalid = || ApiError(StatusCode::BAD_REQUEST, "Передайте один файл в поле file");
    let multipart_error = |error: axum::extract::multipart::MultipartError| {
        ApiError(error.status(), "Не удалось прочитать загрузку")
    };
    let field = multipart
        .next_field()
        .await
        .map_err(multipart_error)?
        .ok_or_else(invalid)?;
    let name = field
        .file_name()
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 255
                && !name.contains(['/', '\\', ':'])
                && !name.chars().any(char::is_control)
        })
        .ok_or_else(invalid)?
        .to_owned();
    if field.name() != Some("file") {
        return Err(invalid());
    }
    let bytes = field.bytes().await.map_err(multipart_error)?;
    if bytes.len() > MAX_UPLOAD {
        return Err(ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Файл превышает 20 МБ",
        ));
    }
    if multipart
        .next_field()
        .await
        .map_err(multipart_error)?
        .is_some()
    {
        return Err(invalid());
    }
    let session = chat.begin()?;
    tokio::spawn(async move {
        chat.run(
            session,
            Input::Document {
                name: Path::new(&name),
                bytes: &bytes,
                answers: Default::default(),
            },
        )
        .await;
    });
    Ok(StatusCode::ACCEPTED)
}

async fn session_events(
    State(app): State<App>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let chat = app.chat(&id)?;
    let cursor = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or_default();
    Ok(Arc::clone(&chat.events).subscribe(cursor))
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Format {
    #[default]
    Json,
    Md,
}

#[derive(Deserialize)]
struct ReportQuery {
    #[serde(default)]
    format: Format,
}

async fn report(
    State(app): State<App>,
    RoutePath(id): RoutePath<String>,
    Query(query): Query<ReportQuery>,
) -> ApiResult<Response> {
    let chat = app.chat(&id)?;
    let session = chat.session.try_lock().map_err(|_| {
        ApiError(
            StatusCode::CONFLICT,
            "В сессии выполняется команда. Повторите запрос после её завершения",
        )
    })?;
    let report = session
        .report()
        .ok_or(ApiError(StatusCode::CONFLICT, "Отчёт ещё не готов"))?;
    let (content_type, content) = match query.format {
        Format::Json => ("application/json", JsonRenderer.render(report)),
        Format::Md => (
            "text/markdown; charset=utf-8",
            MarkdownRenderer.render(report),
        ),
    };
    let content = content.map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Не удалось построить отчёт",
        )
    })?;
    Ok(([(header::CONTENT_TYPE, content_type)], content).into_response())
}
