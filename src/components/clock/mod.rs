use axum::{Router, http::header, response::IntoResponse, routing::get};

use crate::components::GridSize;

use super::dock_component;

dock_component!(Clock, "clock", GridSize::ONE_BY_ONE, routes());

fn routes() -> Router {
    Router::new().route("/component.js", get(component_javascript))
}

async fn component_javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("component.js"),
    )
}
