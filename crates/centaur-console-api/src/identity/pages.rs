//! The server-rendered pages: `GET/POST /login`, `POST|DELETE /logout`,
//! `GET /pending`, `GET /launch`. They are public routes, so each handler
//! reads the session itself.

use askama::Template;
use axum::{body::Bytes, extract::State, http::HeaderMap, response::Response};
use centaur_console_auth::{denial::DISABLED_MESSAGE, return_path};
use http::StatusCode;
use serde::Deserialize;

use super::{landing_path, pwa, settings};
use crate::{
    ApiResult, AppState,
    assets::Assets,
    auth::{Session, session_user, verify_form_csrf},
    extract::{Form, Query},
    pages::{redirect, render},
};

#[derive(Template)]
#[template(path = "pages/login.html")]
struct LoginPage<'a> {
    assets: &'a Assets,
    notice: Option<String>,
    alert: Option<String>,
    providers: Vec<(&'static str, String)>,
    password_login: bool,
    csrf: String,
}

#[derive(Template)]
#[template(path = "pages/pending.html")]
struct PendingPage<'a> {
    assets: &'a Assets,
    notice: Option<String>,
    email: &'a str,
    csrf: String,
}

fn login_page(
    state: &AppState,
    session: &Session,
    status: StatusCode,
    alert: Option<&str>,
) -> Response {
    let (flash, csrf) = {
        let mut session = session.lock();
        (session.take_flash(), session.csrf_token().to_owned())
    };
    let env = &state.config.env;
    render(
        status,
        &LoginPage {
            assets: &state.assets,
            notice: flash.notice,
            alert: alert.map(str::to_owned).or(flash.alert),
            providers: settings::providers(env)
                .into_iter()
                .map(|provider| (provider, settings::capitalize(provider)))
                .collect(),
            password_login: settings::password_login_enabled(env),
            csrf,
        },
    )
}

#[derive(Deserialize)]
pub struct LoginQuery {
    return_to: Option<String>,
    next: Option<String>,
}

/// `GET /login`. A signed-in active user goes to the allowlisted
/// `?return_to=` (or `?next=`) or their landing page; a signed-out
/// browser's `return_to` is remembered for after sign-in.
pub async fn login_form(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<LoginQuery>,
) -> ApiResult {
    let user = session_user(&state, &session).await?;
    let requested = query
        .return_to
        .as_deref()
        .filter(|path| !path.is_empty())
        .or(query.next.as_deref())
        .and_then(return_path::safe);
    match user {
        Some(user) if user.is_active() => Ok(redirect(
            &requested.unwrap_or_else(|| landing_path(user.admin).to_owned()),
        )),
        Some(_) => Ok(redirect("/pending")),
        None => {
            if let Some(path) = query.return_to.as_deref().and_then(return_path::safe) {
                session.lock().set_return_to(path);
            }
            Ok(login_page(&state, &session, StatusCode::OK, None))
        }
    }
}

#[derive(Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    authenticity_token: Option<String>,
}

#[derive(sqlx::FromRow)]
struct Credentials {
    id: i64,
    admin: bool,
    status: String,
    password_digest: Option<String>,
}

/// `POST /login`: email and password.
pub async fn login(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> ApiResult {
    verify_form_csrf(
        &state,
        &session,
        &headers,
        form.authenticity_token.as_deref(),
    )?;
    if !settings::password_login_enabled(&state.config.env) {
        return Ok(login_page(
            &state,
            &session,
            StatusCode::NOT_FOUND,
            Some("Email and password sign in is disabled."),
        ));
    }
    let email = form.email.trim().to_lowercase();
    let user: Option<Credentials> =
        sqlx::query_as("SELECT id, admin, status, password_digest FROM users WHERE email = $1")
            .bind(&email)
            .fetch_optional(&state.db)
            .await?;
    let digest = user
        .as_ref()
        .and_then(|user| user.password_digest.as_deref());
    let verified = centaur_console_auth::password::verify(&form.password, digest).await;
    let Some(user) = user.filter(|_| verified) else {
        return Ok(login_page(
            &state,
            &session,
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("Invalid email or password."),
        ));
    };
    if user.status == "disabled" {
        return Ok(login_page(
            &state,
            &session,
            StatusCode::UNPROCESSABLE_ENTITY,
            Some(DISABLED_MESSAGE),
        ));
    }
    Ok(sign_in(
        &state,
        &session,
        user.id,
        user.admin,
        user.status == "active",
        &email,
    ))
}

/// Signs `user_id` in (a new session, no fixation) and redirects to the
/// remembered return path or the landing page; a pending account goes to
/// the holding page and keeps its return path for after approval.
pub fn sign_in(
    state: &AppState,
    session: &Session,
    user_id: i64,
    admin: bool,
    active: bool,
    email: &str,
) -> Response {
    let mut session = session.lock();
    session.sign_in(user_id, state.now());
    if active {
        session.set_notice(format!("Signed in as {email}."));
        let target = session
            .take_return_to()
            .unwrap_or_else(|| landing_path(admin).to_owned());
        redirect(&target)
    } else {
        session.set_notice("Your account is awaiting approval.");
        redirect("/pending")
    }
}

#[derive(Deserialize, Default)]
pub struct TokenForm {
    authenticity_token: Option<String>,
}

/// `POST /logout` (and `DELETE /logout`).
pub async fn logout(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    // The form's token, or the `X-CSRF-Token` header for a bodiless DELETE.
    let form: TokenForm = serde_urlencoded::from_bytes(&body).unwrap_or_default();
    verify_form_csrf(
        &state,
        &session,
        &headers,
        form.authenticity_token.as_deref(),
    )?;
    if session_user(&state, &session).await?.is_none() {
        return Ok(redirect("/login"));
    }
    let mut session = session.lock();
    session.sign_out(state.now());
    session.set_notice("Signed out.");
    Ok(redirect("/login"))
}

/// `GET /pending`: the holding page for an account awaiting approval.
pub async fn pending(State(state): State<AppState>, session: Session) -> ApiResult {
    let Some(user) = session_user(&state, &session).await? else {
        return Ok(redirect("/login"));
    };
    if user.is_active() {
        return Ok(redirect(landing_path(user.admin)));
    }
    let (flash, csrf) = {
        let mut session = session.lock();
        (session.take_flash(), session.csrf_token().to_owned())
    };
    Ok(render(
        StatusCode::OK,
        &PendingPage {
            assets: &state.assets,
            notice: flash.notice,
            email: &user.email,
            csrf,
        },
    ))
}

#[derive(Deserialize)]
pub struct LaunchQuery {
    #[serde(default)]
    target: String,
}

/// `GET /launch?target=web+centaur://console/...` (the PWA protocol
/// handler): signed-in active users only.
pub async fn launch(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<LaunchQuery>,
) -> ApiResult {
    match session_user(&state, &session).await? {
        None => Ok(redirect("/login")),
        Some(user) if !user.is_active() => Ok(redirect("/pending")),
        Some(_) => Ok(redirect(&pwa::launch_path(&query.target))),
    }
}
