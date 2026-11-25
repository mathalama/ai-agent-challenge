use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use agent::{Input, Provider, Session, build_provider};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path as RoutePath, Query, Request, State},
    http::{HeaderMap, StatusCode, header, uri::Authority},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
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
    provider: Provider,
    no_cache: bool,
}

struct SessionEntry {
    last_used: Instant,
    session: Arc<Chat>,
}

struct Chat {
    session: Arc<AsyncMutex<Session>>,
    events: Arc<Events>,
}

type ApiResult<T> = std::result::Result<T, ApiError>;

struct ApiError(StatusCode, &'static str);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

pub async fn serve(
    address: SocketAddr,
    provider: Provider,
    no_cache: bool,
    web_dir: PathBuf,
) -> agent::Result<()> {
    let listener = tokio::net::TcpListener::bind(address).await?;
    let router = router(provider, no_cache, &web_dir, address.ip().is_loopback());
    println!("Сервер доступен на http://{}", listener.local_addr()?);
    axum::serve(listener, router).await?;
    Ok(())
}

fn router(provider: Provider, no_cache: bool, web_dir: &Path, loopback: bool) -> Router {
    let app = App {
        sessions: Arc::default(),
        provider,
        no_cache,
    };
    let api = Router::new()
        .route("/schema", get(|| async { Json(Report::json_schema()) }))
        .route("/sessions", post(create_session))
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
    Router::new().nest("/api", api).fallback_service(
        ServeDir::new(web_dir).fallback(ServeFile::new(web_dir.join("index.html"))),
    )
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
        entry.last_used.elapsed() < SESSION_IDLE || entry.session.session.try_lock().is_err()
    });
    if sessions.len() >= MAX_SESSIONS {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Открыто слишком много сессий",
        ));
    }
    let id = Uuid::new_v4();
    let App {
        provider, no_cache, ..
    } = app;
    let session =
        Session::new(move |trace, reasoning| build_provider(provider, no_cache, trace, reasoning));
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
        Json(json!({ "id": id.to_string(), "greeting": Session::greeting() })),
    ))
}

impl App {
    fn chat(&self, id: &str) -> ApiResult<Arc<Chat>> {
        let missing = || ApiError(StatusCode::NOT_FOUND, "Сессия не найдена");
        let id = Uuid::parse_str(id).map_err(|_| missing())?;
        let mut sessions = self.sessions.lock().expect("блокировка сессий");
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
    let session = chat.session.lock().await;
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
