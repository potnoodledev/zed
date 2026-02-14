pub mod events;
pub mod extensions;

use crate::{AppState, Error, Result, auth, db::UserId, rpc};
use anyhow::Context as _;
use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{Path, Query},
    headers::Header,
    http::{self, HeaderName, Request, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect},
    routing::{get, post},
};
use axum_extra::response::ErasedJson;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use tower::ServiceBuilder;

pub use extensions::fetch_extensions_from_blob_store_periodically;

pub struct CloudflareIpCountryHeader(String);

impl Header for CloudflareIpCountryHeader {
    fn name() -> &'static HeaderName {
        static CLOUDFLARE_IP_COUNTRY_HEADER: OnceLock<HeaderName> = OnceLock::new();
        CLOUDFLARE_IP_COUNTRY_HEADER.get_or_init(|| HeaderName::from_static("cf-ipcountry"))
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, axum::headers::Error>
    where
        Self: Sized,
        I: Iterator<Item = &'i axum::http::HeaderValue>,
    {
        let country_code = values
            .next()
            .ok_or_else(axum::headers::Error::invalid)?
            .to_str()
            .map_err(|_| axum::headers::Error::invalid())?;

        Ok(Self(country_code.to_string()))
    }

    fn encode<E: Extend<axum::http::HeaderValue>>(&self, _values: &mut E) {
        unimplemented!()
    }
}

impl std::fmt::Display for CloudflareIpCountryHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub struct SystemIdHeader(String);

impl Header for SystemIdHeader {
    fn name() -> &'static HeaderName {
        static SYSTEM_ID_HEADER: OnceLock<HeaderName> = OnceLock::new();
        SYSTEM_ID_HEADER.get_or_init(|| HeaderName::from_static("x-zed-system-id"))
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, axum::headers::Error>
    where
        Self: Sized,
        I: Iterator<Item = &'i axum::http::HeaderValue>,
    {
        let system_id = values
            .next()
            .ok_or_else(axum::headers::Error::invalid)?
            .to_str()
            .map_err(|_| axum::headers::Error::invalid())?;

        Ok(Self(system_id.to_string()))
    }

    fn encode<E: Extend<axum::http::HeaderValue>>(&self, _values: &mut E) {
        unimplemented!()
    }
}

impl std::fmt::Display for SystemIdHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub fn routes(rpc_server: Arc<rpc::Server>) -> Router<(), Body> {
    Router::new()
        .route("/users/:id/access_tokens", post(create_access_token))
        .route("/rpc_server_snapshot", get(get_rpc_server_snapshot))
        .route(
            "/internal/users/impersonate",
            post(impersonate_user),
        )
        .layer(
            ServiceBuilder::new()
                .layer(Extension(rpc_server))
                .layer(middleware::from_fn(validate_api_token)),
        )
}

pub fn public_routes() -> Router<(), Body> {
    Router::new()
        .route("/native_app_signin", get(native_app_signin))
        .route("/native_app_signin_succeeded", get(native_app_signin_succeeded))
        .route("/auth/github_callback", get(github_callback))
        .route("/auth/affine_callback", get(affine_callback))
}

pub async fn validate_api_token<B>(req: Request<B>, next: Next<B>) -> impl IntoResponse {
    let header_value = req
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|header| header.to_str().ok())
        .ok_or_else(|| {
            Error::http(
                StatusCode::BAD_REQUEST,
                "missing authorization header".to_string(),
            )
        })?;

    let token = header_value
        .strip_prefix("token ")
        .or_else(|| header_value.strip_prefix("Bearer "))
        .ok_or_else(|| {
            Error::http(
                StatusCode::BAD_REQUEST,
                "invalid authorization header".to_string(),
            )
        })?;

    let state = req.extensions().get::<Arc<AppState>>().unwrap();

    if token != state.config.api_token {
        Err(Error::http(
            StatusCode::UNAUTHORIZED,
            "invalid authorization token".to_string(),
        ))?
    }

    Ok::<_, Error>(next.run(req).await)
}

async fn get_rpc_server_snapshot(
    Extension(rpc_server): Extension<Arc<rpc::Server>>,
) -> Result<ErasedJson> {
    Ok(ErasedJson::pretty(rpc_server.snapshot().await))
}

#[derive(Deserialize)]
struct CreateAccessTokenQueryParams {
    public_key: String,
    impersonate: Option<String>,
}

#[derive(Serialize)]
struct CreateAccessTokenResponse {
    user_id: UserId,
    encrypted_access_token: String,
}

async fn create_access_token(
    Path(user_id): Path<UserId>,
    Query(params): Query<CreateAccessTokenQueryParams>,
    Extension(app): Extension<Arc<AppState>>,
) -> Result<Json<CreateAccessTokenResponse>> {
    let user = app
        .db
        .get_user_by_id(user_id)
        .await?
        .context("user not found")?;

    let mut impersonated_user_id = None;
    if let Some(impersonate) = params.impersonate {
        if user.admin {
            if let Some(impersonated_user) = app.db.get_user_by_github_login(&impersonate).await? {
                impersonated_user_id = Some(impersonated_user.id);
            } else {
                return Err(Error::http(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("user {impersonate} does not exist"),
                ));
            }
        } else {
            return Err(Error::http(
                StatusCode::UNAUTHORIZED,
                "you do not have permission to impersonate other users".to_string(),
            ));
        }
    }

    let access_token =
        auth::create_access_token(app.db.as_ref(), user_id, impersonated_user_id).await?;
    let encrypted_access_token =
        auth::encrypt_access_token(&access_token, params.public_key.clone())?;

    Ok(Json(CreateAccessTokenResponse {
        user_id: impersonated_user_id.unwrap_or(user_id),
        encrypted_access_token,
    }))
}

#[derive(Deserialize)]
struct ImpersonateUserRequest {
    github_login: String,
}

#[derive(Serialize)]
struct ImpersonateUserResponse {
    user_id: u64,
    access_token: String,
}

async fn impersonate_user(
    Extension(app): Extension<Arc<AppState>>,
    Json(request): Json<ImpersonateUserRequest>,
) -> Result<Json<ImpersonateUserResponse>> {
    let user = if let Some(user) = app.db.get_user_by_github_login(&request.github_login).await? {
        user
    } else if let Some(user) = app.db.get_user_by_affine_id(&request.github_login).await? {
        user
    } else if let Some(user) = app.db.get_user_by_email(&request.github_login).await? {
        user
    } else {
        return Err(Error::http(
            StatusCode::NOT_FOUND,
            format!("user {} not found", request.github_login),
        ));
    };

    let access_token =
        auth::create_access_token(app.db.as_ref(), user.id, None).await?;

    Ok(Json(ImpersonateUserResponse {
        user_id: user.id.0 as u64,
        access_token,
    }))
}

#[derive(Deserialize)]
struct NativeAppSigninParams {
    native_app_port: Option<u16>,
    native_app_public_key: Option<String>,
}

async fn native_app_signin(
    Query(params): Query<NativeAppSigninParams>,
    Extension(app): Extension<Arc<AppState>>,
) -> Result<Html<String>> {
    let port = params.native_app_port.unwrap_or(0);
    let public_key = params.native_app_public_key.clone().unwrap_or_default();

    let github_enabled = app.config.github_client_id.as_ref().is_some_and(|s| !s.is_empty());
    let affine_enabled = app.config.affine_url.as_ref().is_some_and(|s| !s.is_empty());

    let github_button = if github_enabled {
        let client_id = app.config.github_client_id.as_ref().map(|s| s.as_str()).unwrap_or_default();
        let state = format!("{port}:{public_key}");
        let state_encoded = urlencoding::encode(&state);
        let redirect_uri = format!(
            "{}/auth/github_callback",
            app.config.self_url()
        );
        let redirect_uri_encoded = urlencoding::encode(&redirect_uri);
        format!(
            r#"<a href="https://github.com/login/oauth/authorize?client_id={client_id}&redirect_uri={redirect_uri_encoded}&state={state_encoded}" class="btn btn-github">Sign in with GitHub</a>"#
        )
    } else {
        String::new()
    };

    let affine_button = if affine_enabled {
        let affine_url = app.config.affine_public_url.as_ref()
            .or(app.config.affine_url.as_ref())
            .map(|s| s.as_str())
            .unwrap_or_default();
        let callback_url = format!(
            "{}/auth/affine_callback",
            app.config.self_url()
        );
        let state = format!("{port}:{public_key}");
        let state_encoded = urlencoding::encode(&state);
        let redirect_uri = format!(
            "{affine_url}/sign-in?redirect_uri={}",
            urlencoding::encode(&format!("{callback_url}?state={state_encoded}"))
        );
        format!(
            r#"<a href="{redirect_uri}" class="btn btn-affine">Sign in with AFFiNE</a>"#
        )
    } else {
        String::new()
    };

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Sign in to Zed</title>
    <style>
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            display: flex;
            justify-content: center;
            align-items: center;
            min-height: 100vh;
            margin: 0;
            background: #1e1e2e;
            color: #cdd6f4;
        }}
        .container {{
            text-align: center;
            padding: 2rem;
        }}
        h1 {{
            margin-bottom: 2rem;
            font-size: 1.5rem;
        }}
        .btn {{
            display: inline-block;
            padding: 12px 24px;
            margin: 8px;
            border-radius: 8px;
            text-decoration: none;
            font-size: 1rem;
            font-weight: 500;
            transition: opacity 0.2s;
        }}
        .btn:hover {{
            opacity: 0.9;
        }}
        .btn-github {{
            background: #333;
            color: #fff;
        }}
        .btn-affine {{
            background: #1e96eb;
            color: #fff;
        }}
    </style>
</head>
<body>
    <div class="container">
        <h1>Sign in to Zed</h1>
        {github_button}
        {affine_button}
    </div>
</body>
</html>"#
    );

    Ok(Html(html))
}

async fn native_app_signin_succeeded() -> Html<&'static str> {
    Html(
        r#"<!DOCTYPE html>
<html>
<head><title>Sign-in Successful</title>
<style>
    body {
        font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
        display: flex;
        justify-content: center;
        align-items: center;
        min-height: 100vh;
        margin: 0;
        background: #1e1e2e;
        color: #cdd6f4;
    }
</style>
</head>
<body>
    <h1>You can close this window and return to Zed.</h1>
</body>
</html>"#,
    )
}

#[derive(Deserialize)]
struct GithubCallbackParams {
    code: String,
    state: String,
}

#[derive(Deserialize)]
struct GithubTokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct GithubUserInfo {
    id: i32,
    login: String,
    email: Option<String>,
    name: Option<String>,
    created_at: String,
}

async fn github_callback(
    Query(params): Query<GithubCallbackParams>,
    Extension(app): Extension<Arc<AppState>>,
) -> Result<impl IntoResponse> {
    let (port_str, public_key) = params
        .state
        .split_once(':')
        .ok_or_else(|| Error::http(StatusCode::BAD_REQUEST, "invalid state".into()))?;
    let port: u16 = port_str
        .parse()
        .map_err(|_| Error::http(StatusCode::BAD_REQUEST, "invalid port in state".into()))?;
    let public_key = public_key.to_string();

    let client_id = app
        .config
        .github_client_id
        .as_ref()
        .ok_or_else(|| Error::http(StatusCode::INTERNAL_SERVER_ERROR, "GitHub OAuth not configured".into()))?;
    let client_secret = app
        .config
        .github_client_secret
        .as_ref()
        .ok_or_else(|| Error::http(StatusCode::INTERNAL_SERVER_ERROR, "GitHub OAuth not configured".into()))?;

    let http_client = reqwest::Client::new();

    let token_response: GithubTokenResponse = http_client
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", &params.code),
        ])
        .send()
        .await
        .map_err(|e| Error::Internal(e.into()))?
        .json()
        .await
        .map_err(|e| Error::Internal(e.into()))?;

    let github_user: GithubUserInfo = http_client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", token_response.access_token))
        .header("User-Agent", "zed-collab")
        .send()
        .await
        .map_err(|e| Error::Internal(e.into()))?
        .json()
        .await
        .map_err(|e| Error::Internal(e.into()))?;

    let created_at = chrono::DateTime::parse_from_rfc3339(&github_user.created_at)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());

    let user = app
        .db
        .update_or_create_user_by_github_account(
            &github_user.login,
            github_user.id,
            github_user.email.as_deref(),
            github_user.name.as_deref(),
            created_at,
            None,
        )
        .await?;

    let access_token = auth::create_access_token(app.db.as_ref(), user.id, None).await?;
    let encrypted_access_token = auth::encrypt_access_token(&access_token, public_key)?;
    let encrypted_access_token_encoded = urlencoding::encode(&encrypted_access_token);

    Ok(Redirect::temporary(&format!(
        "http://127.0.0.1:{port}/?user_id={}&access_token={encrypted_access_token_encoded}",
        user.id
    )))
}

#[derive(Deserialize)]
struct AffineCallbackParams {
    code: String,
    state: String,
}

#[derive(Deserialize)]
struct AffineExchangeResponse {
    id: String,
    email: String,
    name: Option<String>,
    avatar_url: Option<String>,
}

async fn affine_callback(
    Query(params): Query<AffineCallbackParams>,
    Extension(app): Extension<Arc<AppState>>,
) -> Result<impl IntoResponse> {
    let (port_str, public_key) = params
        .state
        .split_once(':')
        .ok_or_else(|| Error::http(StatusCode::BAD_REQUEST, "invalid state".into()))?;
    let port: u16 = port_str
        .parse()
        .map_err(|_| Error::http(StatusCode::BAD_REQUEST, "invalid port in state".into()))?;
    let public_key = public_key.to_string();

    let affine_url = app
        .config
        .affine_url
        .as_ref()
        .ok_or_else(|| Error::http(StatusCode::INTERNAL_SERVER_ERROR, "AFFiNE auth not configured".into()))?;

    let http_client = reqwest::Client::new();

    let affine_user: AffineExchangeResponse = http_client
        .post(format!("{affine_url}/api/auth/open-app/exchange"))
        .json(&serde_json::json!({ "code": params.code }))
        .send()
        .await
        .map_err(|e| Error::Internal(e.into()))?
        .error_for_status()
        .map_err(|e| Error::Internal(e.into()))?
        .json()
        .await
        .map_err(|e| Error::Internal(e.into()))?;

    let user = app
        .db
        .update_or_create_user_by_affine_account(
            &affine_user.id,
            &affine_user.email,
            affine_user.name.as_deref(),
            affine_user.avatar_url.as_deref(),
        )
        .await?;

    let access_token = auth::create_access_token(app.db.as_ref(), user.id, None).await?;
    let encrypted_access_token = auth::encrypt_access_token(&access_token, public_key)?;
    let encrypted_access_token_encoded = urlencoding::encode(&encrypted_access_token);

    Ok(Redirect::temporary(&format!(
        "http://127.0.0.1:{port}/?user_id={}&access_token={encrypted_access_token_encoded}",
        user.id
    )))
}
