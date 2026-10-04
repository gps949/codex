//! Paired browser access to administration only; the native app-server listener is unchanged.

use super::*;
use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::Request;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Html;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use std::future::IntoFuture;
use std::net::SocketAddr;
use std::time::Instant;

#[path = "web_lifecycle.rs"]
mod lifecycle;
use lifecycle::WebLifecycle;

/// Explicit browser origins are required when binding beyond loopback.
pub struct AccountManagerWebOptions {
    pub listen: SocketAddr,
    pub allowed_origins: Vec<String>,
}

#[derive(Clone)]
struct WebState {
    manager: Arc<AccountManager>,
    token: Arc<String>,
    origins: Arc<Vec<String>>,
    lifecycle: WebLifecycle,
}

/// Serves the embedded interface and calls `ready` with a one-time pairing URL.
pub async fn serve(
    manager: Arc<AccountManager>,
    options: AccountManagerWebOptions,
    ready: impl FnOnce(&str),
) -> anyhow::Result<()> {
    anyhow::ensure!(
        options.listen.ip().is_loopback() || !options.allowed_origins.is_empty(),
        "Remote management requires an explicit allowed browser origin"
    );
    let listener = tokio::net::TcpListener::bind(options.listen).await?;
    let address = listener.local_addr()?;
    let local_url = format!("http://{address}");
    let mut origins = Vec::new();
    if address.ip().is_loopback() {
        origins.push(local_url.clone());
        origins.push(format!("http://localhost:{}", address.port()));
    }
    for origin in options.allowed_origins {
        let url = url::Url::parse(&origin)?;
        anyhow::ensure!(
            url.scheme() == "https"
                || address.ip().is_loopback()
                    && url.scheme() == "http"
                    && url
                        .host_str()
                        .is_some_and(|host| host == "localhost" || host == "127.0.0.1"),
            "Remote browser origins must use HTTPS"
        );
        anyhow::ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "Specify only the browser origin"
        );
        origins.push(url.origin().ascii_serialization());
    }
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let state = WebState {
        manager: Arc::clone(&manager),
        token: Arc::new(token),
        origins: Arc::new(origins),
        lifecycle: WebLifecycle::new(Instant::now()),
    };
    let app = router(state.clone());
    ready(&format!("{}/#pair={}", state.origins[0], state.token));
    let stop = state.lifecycle.shutdown();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(stop.clone().cancelled_owned())
        .into_future();
    tokio::pin!(server);
    let result = tokio::select! {
        result = &mut server => result,
        _ = async {
            tokio::select! {
                _ = state.lifecycle.wait() => {},
                _ = exit_signal() => {},
            }
        } => {
            stop.cancel();
            // Cancel device-login waits before HTTP drain, then let credential transactions finish.
            manager.shutdown_logins().await;
            server.await
        }
    };
    manager.shutdown_logins().await;
    result.map_err(Into::into)
}

fn router(state: WebState) -> Router {
    let api = Router::new()
        .route("/inventory", get(inventory))
        .route("/decision-advisor", get(decision_advisor))
        .route("/preferences", get(preferences).post(save_preferences))
        .route("/operation", post(operation))
        .route("/heartbeat", post(heartbeat))
        .route("/leave", post(leave))
        .route("/shutdown", post(shutdown))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            authorize,
        ));
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("webui/index.html")) }),
        )
        .route(
            "/decision.js",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("webui/decision.js"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("webui/app.js"),
                )
            }),
        )
        .route(
            "/messages.js",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("webui/messages.js"),
                )
            }),
        )
        .route(
            "/guidance.js",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("webui/guidance.js"),
                )
            }),
        )
        .route(
            "/lifecycle.js",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("webui/lifecycle.js"),
                )
            }),
        )
        .route(
            "/app.css",
            get(|| async {
                (
                    [("content-type", "text/css; charset=utf-8")],
                    include_str!("webui/app.css"),
                )
            }),
        )
        .route(
            "/primary.js",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("webui/primary.js"),
                )
            }),
        )
        .route(
            "/warmup.js",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("webui/warmup.js"),
                )
            }),
        )
        .route("/api/session", post(pair))
        .nest("/api", api)
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(axum::middleware::from_fn(browser_headers))
        .with_state(state)
}

fn origin_allowed(headers: &HeaderMap, state: &WebState) -> bool {
    let Some(host) = headers.get("host").and_then(|header| header.to_str().ok()) else {
        return false;
    };
    if !state.origins.iter().any(|origin| {
        url::Url::parse(origin)
            .ok()
            .is_some_and(|url| url.authority() == host)
    }) {
        return false;
    }
    headers
        .get("origin")
        .and_then(|origin| origin.to_str().ok())
        .is_none_or(|origin| state.origins.iter().any(|allowed| origin == allowed))
}

fn token_matches(actual: &str, expected: &str) -> bool {
    actual.len() == expected.len()
        && actual
            .bytes()
            .zip(expected.bytes())
            .fold(0_u8, |different, (a, b)| different | (a ^ b))
            == 0
}

async fn authorize(State(state): State<WebState>, request: Request, next: Next) -> Response {
    let token = request
        .headers()
        .get("x-codex-pool-token")
        .and_then(|value| value.to_str().ok());
    if !origin_allowed(request.headers(), &state)
        || !token.is_some_and(|token| token_matches(token, &state.token))
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"Pair this browser with the account manager first"})),
        )
            .into_response();
    }
    if state.lifecycle.shutdown().is_cancelled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"Account manager is shutting down"})),
        )
            .into_response();
    }
    let _activity = match request.uri().path() {
        "/inventory" | "/operation" | "/preferences" | "/api/inventory" | "/api/operation"
        | "/api/preferences" => match state.lifecycle.activity() {
            Ok(activity) => Some(activity),
            Err(error) => return api_error(error),
        },
        _ => None,
    };
    next.run(request).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairRequest {
    token: String,
    client_id: Option<String>,
}

async fn pair(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<PairRequest>,
) -> Response {
    if !origin_allowed(&headers, &state) || !token_matches(&request.token, &state.token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if state.lifecycle.shutdown().is_cancelled() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if let Some(tab) = request.client_id
        && let Err(error) = state.lifecycle.renew(&tab, Instant::now())
    {
        return api_error(error);
    }
    Json(serde_json::json!({"paired":true,"sessionToken":state.token.as_str()})).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TabRequest {
    client_id: String,
}

async fn heartbeat(State(state): State<WebState>, Json(request): Json<TabRequest>) -> Response {
    match state.lifecycle.renew(&request.client_id, Instant::now()) {
        Ok(()) => Json(serde_json::json!({"alive":true})).into_response(),
        Err(error) => api_error(error),
    }
}

async fn leave(State(state): State<WebState>, Json(request): Json<TabRequest>) -> Response {
    match state.lifecycle.leave(&request.client_id, Instant::now()) {
        Ok(()) => Json(serde_json::json!({"left":true})).into_response(),
        Err(error) => api_error(error),
    }
}

async fn shutdown(State(state): State<WebState>) -> Response {
    state.lifecycle.shutdown().cancel();
    Json(serde_json::json!({"stopped":true})).into_response()
}

async fn exit_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

async fn inventory(State(state): State<WebState>) -> Response {
    match state.manager.inventory().await {
        Ok(inventory) => Json(inventory).into_response(),
        Err(error) => api_error(error),
    }
}

async fn preferences(State(state): State<WebState>) -> Response {
    Json(state.manager.preferences()).into_response()
}

async fn save_preferences(
    State(state): State<WebState>,
    Json(preferences): Json<ManagerPreferences>,
) -> Response {
    match state.manager.save_preferences(preferences) {
        Ok(()) => Json(preferences).into_response(),
        Err(error) => api_error(error),
    }
}

async fn decision_advisor(State(state): State<WebState>) -> Response {
    match state.manager.decision_advisor_view().await {
        Ok(view) => Json(view).into_response(),
        Err(error) => api_error(error),
    }
}

async fn operation(
    State(state): State<WebState>,
    Json(operation): Json<AccountManagerOperation>,
) -> Response {
    let result = match operation {
        operation @ (AccountManagerOperation::Refresh { .. }
        | AccountManagerOperation::Credits { .. }
        | AccountManagerOperation::DecisionProbe { .. }) => {
            tokio::select! {
                result = state.manager.execute(operation) => result,
                _ = state.lifecycle.shutdown().cancelled_owned() => {
                    Err(anyhow::anyhow!("Account manager is shutting down"))
                }
            }
        }
        operation => state.manager.execute(operation).await,
    };
    match result {
        Ok(result) => Json(result).into_response(),
        Err(error) => api_error(error),
    }
}

fn api_error(error: anyhow::Error) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"error":error.to_string()})),
    )
        .into_response()
}

async fn browser_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; form-action 'self'; base-uri 'none'",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}

#[cfg(test)]
#[path = "web_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "web_preferences_tests.rs"]
mod preferences_tests;
