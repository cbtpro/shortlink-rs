//! `/healthz` 与 `/readyz`。
//!
//! 存活检查不碰依赖（进程活着就该是 200），就绪检查才探测 MySQL / Redis：
//! 用它们区分「进程还在但依赖断了」，这正是容器编排需要的信号。

use actix_web::{get, web, HttpResponse};
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
struct HealthBody {
    status: &'static str,
    profile: &'static str,
    assets: &'static str,
    stamp: &'static str,
}

#[get("/healthz")]
pub async fn healthz(state: web::Data<AppState>) -> HttpResponse {
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .json(HealthBody {
            status: "ok",
            profile: state.profile.as_str(),
            assets: state.assets.mode().as_str(),
            stamp: crate::assets::ASSET_STAMP,
        })
}

#[derive(Serialize)]
struct ReadyBody {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    failed: Option<Vec<&'static str>>,
}

#[get("/readyz")]
pub async fn readyz(state: web::Data<AppState>) -> HttpResponse {
    let Some(readiness) = &state.readiness else {
        // 只有测试替身会缺 probe；生产装配一定会注入
        return HttpResponse::ServiceUnavailable()
            .insert_header(("Cache-Control", "no-store"))
            .json(ReadyBody {
                status: "unavailable",
                failed: None,
            });
    };

    let failed = readiness.failed_checks().await;
    if failed.is_empty() {
        HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store"))
            .json(ReadyBody {
                status: "ready",
                failed: None,
            })
    } else {
        HttpResponse::ServiceUnavailable()
            .insert_header(("Cache-Control", "no-store"))
            .json(ReadyBody {
                status: "degraded",
                failed: Some(failed),
            })
    }
}
