//! HTTP API of an agent. The same API serves peers (gossip, bundle transfer)
//! and the operator UI.
//!
//! | route                | purpose                                              |
//! |----------------------|------------------------------------------------------|
//! | `GET  /`             | operator web UI                                      |
//! | `GET  /health`       | liveness probe                                       |
//! | `GET  /state`        | this node's view of the fleet                        |
//! | `POST /state`        | gossip: merge a peer's state, answer with ours       |
//! | `GET  /bundles`      | releases stored on this node                         |
//! | `POST /bundles`      | store a release (verified, content-addressed)        |
//! | `GET  /bundles/{h}`  | download a release (peers use this)                  |
//! | `POST /desired`      | act as operator: set the desired release             |
//! | `POST /busy`         | safety gate: postpone updates while busy             |
//! | `GET  /events`       | this node's recent events                            |
//! | `GET  /api/fleet`    | everything the UI needs in one call                  |
//!
//! There is no authentication yet. Run it on a trusted network only.

use crate::node::{Event, Node, RobotView};
use crate::release::{short, Bundle};
use crate::state::{Desired, FleetState};
use crate::store::BundleInfo;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const INDEX_HTML: &str = include_str!("../ui/index.html");
const MAX_ROBOTS_PER_MESSAGE: usize = 1000;

type Shared = Arc<Node>;

pub fn router(node: Arc<Node>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(|| async { "ok" }))
        .route("/state", get(get_state).post(post_state))
        .route("/bundles", get(list_bundles).post(post_bundle))
        .route("/bundles/{hash}", get(get_bundle))
        .route("/desired", post(post_desired))
        .route("/busy", post(post_busy))
        .route("/events", get(get_events))
        .route("/api/fleet", get(api_fleet))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .with_state(node)
}

struct AppError(StatusCode, String);

impl AppError {
    fn bad(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn get_state(State(n): State<Shared>) -> Json<FleetState> {
    Json(n.snapshot())
}

async fn post_state(
    State(n): State<Shared>,
    Json(remote): Json<FleetState>,
) -> Result<Json<FleetState>, AppError> {
    if remote.robots.len() > MAX_ROBOTS_PER_MESSAGE {
        return Err(AppError::bad("too many robots in one message"));
    }
    n.merge_remote(&remote);
    Ok(Json(n.snapshot()))
}

async fn list_bundles(State(n): State<Shared>) -> Result<Json<Vec<BundleInfo>>, AppError> {
    n.store
        .list_bundles()
        .map(Json)
        .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

async fn get_bundle(
    State(n): State<Shared>,
    Path(hash): Path<String>,
) -> Result<Json<Bundle>, AppError> {
    match n.store.get_bundle(&hash) {
        Ok(Some(b)) => Ok(Json(b)),
        Ok(None) => Err(AppError(StatusCode::NOT_FOUND, "unknown release".into())),
        Err(e) => Err(AppError(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{e:#}"),
        )),
    }
}

#[derive(Serialize)]
struct Stored {
    hash: String,
}

async fn post_bundle(
    State(n): State<Shared>,
    Json(bundle): Json<Bundle>,
) -> Result<Json<Stored>, AppError> {
    let hash = n
        .store
        .put_bundle(&bundle)
        .map_err(|e| AppError::bad(format!("{e:#}")))?;
    n.event(
        "info",
        format!(
            "stored release {} {}",
            short(&hash),
            bundle.label.as_deref().unwrap_or("")
        ),
    );
    Ok(Json(Stored { hash }))
}

#[derive(Deserialize)]
struct DesiredRequest {
    release_hash: Option<String>,
    canary: Option<String>,
}

async fn post_desired(
    State(n): State<Shared>,
    Json(req): Json<DesiredRequest>,
) -> Result<Json<Desired>, AppError> {
    if let Some(h) = &req.release_hash {
        if !n.store.has_bundle(h) {
            return Err(AppError::bad(
                "unknown release: publish the bundle to this node first (agent publish ...)",
            ));
        }
    }
    if let Some(c) = &req.canary {
        if !n.snapshot().robots.contains_key(c) {
            return Err(AppError::bad(format!("unknown canary robot {c:?}")));
        }
    }
    let d = n.set_desired(req.release_hash, req.canary);
    n.event(
        "info",
        format!(
            "desired release set to {}{} (#{})",
            d.release_hash.as_deref().map(short).unwrap_or("none"),
            d.canary
                .as_deref()
                .map(|c| format!(", canary {c}"))
                .unwrap_or_default(),
            d.counter
        ),
    );
    Ok(Json(d))
}

#[derive(Deserialize)]
struct BusyRequest {
    busy: bool,
}

async fn post_busy(State(n): State<Shared>, Json(req): Json<BusyRequest>) -> &'static str {
    n.mutate_own(|o| o.busy = req.busy);
    n.event(
        "info",
        if req.busy {
            "marked busy: updates are postponed"
        } else {
            "marked idle"
        },
    );
    n.notify.notify_one();
    "ok"
}

async fn get_events(State(n): State<Shared>) -> Json<Vec<Event>> {
    Json(n.events())
}

#[derive(Serialize, Deserialize)]
pub struct FleetView {
    pub self_id: String,
    pub desired: Desired,
    pub robots: Vec<RobotView>,
    pub bundles: Vec<BundleInfo>,
}

async fn api_fleet(State(n): State<Shared>) -> Json<FleetView> {
    Json(FleetView {
        self_id: n.cfg.id.clone(),
        desired: n.snapshot().desired,
        robots: n.robot_views(),
        bundles: n.store.list_bundles().unwrap_or_default(),
    })
}
