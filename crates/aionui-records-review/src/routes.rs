// `ApiError` is the intended error type at this HTTP boundary.
#![allow(clippy::disallowed_types)]

//! - `POST /api/records-review/compute` — package JSON in, [`ComponentReview`] out.
//! - `GET /api/records-review/parts-tree?part_number=&asset_model=` — knowledge-base tree only.

use aionui_api_types::ApiResponse;
use aionui_common::ApiError;
use axum::extract::Query;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::engine::review;
use crate::knowledge::build_tree;
use crate::lookup::{LookupRequest, lookup_from_package};
use crate::model::{ComponentIdentity, ComponentReview, PACKAGE_SCHEMA, PartTree, ReviewPackage};

pub fn records_review_routes() -> Router {
    Router::new()
        .route("/api/records-review/compute", post(compute))
        .route("/api/records-review/lookup", post(lookup))
        .route("/api/records-review/parts-tree", get(parts_tree))
}

async fn compute(Json(pkg): Json<ReviewPackage>) -> Result<Json<ApiResponse<ComponentReview>>, ApiError> {
    if !pkg.schema.is_empty() && pkg.schema != PACKAGE_SCHEMA {
        return Err(ApiError::BadRequest(format!(
            "unsupported schema `{}`, expected `{PACKAGE_SCHEMA}`",
            pkg.schema
        )));
    }
    if pkg.component.part_number.trim().is_empty() || pkg.component.serial_number.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "component.part_number and component.serial_number are required".into(),
        ));
    }
    let today = chrono::Local::now().date_naive();
    Ok(Json(ApiResponse::ok(review(&pkg, today))))
}

async fn lookup(Json(body): Json<LookupRequest>) -> Result<Json<ApiResponse<crate::WorkbenchLookupCard>>, ApiError> {
    if !body.package.schema.is_empty() && body.package.schema != PACKAGE_SCHEMA {
        return Err(ApiError::BadRequest(format!(
            "unsupported schema `{}`, expected `{PACKAGE_SCHEMA}`",
            body.package.schema
        )));
    }
    if body.package.component.part_number.trim().is_empty() || body.package.component.serial_number.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "component.part_number and component.serial_number are required".into(),
        ));
    }
    let today = chrono::Local::now().date_naive();
    Ok(Json(ApiResponse::ok(lookup_from_package(
        &body.package,
        &body.query,
        today,
    ))))
}

#[derive(Deserialize)]
struct TreeQuery {
    #[serde(default)]
    part_number: String,
    #[serde(default)]
    serial_number: String,
    #[serde(default)]
    asset_model: String,
}

async fn parts_tree(Query(q): Query<TreeQuery>) -> Json<ApiResponse<PartTree>> {
    let component = ComponentIdentity {
        part_number: q.part_number,
        serial_number: q.serial_number,
        asset_model: q.asset_model,
        ..Default::default()
    };
    Json(ApiResponse::ok(build_tree(&component, &[], &[])))
}
