//! 服务冒烟集成测试：catalog 加载 / 注册表构建 / 路由选路 / HTTP oneshot（不监听真实端口）。
use aigw_panel::catalog::Catalog;
use aigw_panel::core::config::Settings;
use aigw_panel::platforms::Registry;
use aigw_panel::router::Router;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower::ServiceExt;

fn mk_state() -> aigw_panel::server::state::AppState {
    let catalog = Catalog::load();
    let router = Arc::new(Router::new());
    router.register_auto_free("https://text.pollinations.ai/openai");
    let tmp = std::env::temp_dir().join("aigw-test-data");
    let registry = Arc::new(Registry::build(Arc::new(catalog.clone()), tmp.clone()));
    aigw_panel::server::state::AppState {
        inner: Arc::new(aigw_panel::server::state::AppStateInner {
            settings: RwLock::new(Settings::default()),
            catalog,
            router,
            registry,
            disabled: RwLock::new(HashSet::new()),
            started: std::time::Instant::now(),
            login: Arc::new(aigw_panel::server::login::LoginManager::new(tmp)),
            usage: Arc::new(aigw_panel::server::usage::UsageRecorder::new()),
            credits_cache: RwLock::new(None),
            pool: Arc::new(aigw_panel::server::pool::Pool::new()),
        }),
    }
}

#[test]
fn catalog_loads_with_platforms() {
    let c = Catalog::load();
    let n = c.platforms().as_object().map(|m| m.len()).unwrap_or(0);
    assert!(n >= 40, "catalog 应至少含 40 个平台，实际 {}", n);
    // 单一数据源关键键存在
    assert!(c.raw_get("bundled").is_some());
    assert!(c.raw_get("relay_specs").is_some());
    assert!(c.raw_get("free_lanes").is_some());
}

#[test]
fn registry_covers_platforms() {
    let c = Arc::new(Catalog::load());
    let r = Registry::build(c.clone(), std::env::temp_dir().join("aigw-test-data"));
    let n_plat = c.platforms().as_object().map(|m| m.len()).unwrap_or(0);
    let n_reg = r.ids().len();
    assert!(
        n_reg >= n_plat,
        "registry 应覆盖 catalog 全部平台（registry={} catalog={}）",
        n_reg,
        n_plat
    );
    // 免凭据车道必须已注册（pollinations 是 Phase-3 永远在线兜底）
    assert!(r.get("pollinations").is_some(), "pollinations 车道必须注册");
}

#[test]
fn grouped_returns_real_groups() {
    let c = Catalog::load();
    let g = c.grouped(&HashSet::new());
    let total = g.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
    let groups = g.get("groups").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    assert!(groups.len() >= 10, "应有 ≥10 个分组，实际 {}", groups.len());
    assert!(total >= 100, "应 ≥100 个模型，实际 {}", total);
}

#[test]
fn router_picks_auto_target() {
    let r = Router::new();
    r.register_auto_free("https://text.pollinations.ai/openai");
    let t = r.pick("auto", "fastest", None);
    assert!(t.is_some(), "auto 必须能选到目标");
    let t = t.unwrap();
    assert_eq!(t.model, "gpt-oss-20b");
    // free 策略应把 free=true 的目标排前
    let t2 = r.pick("free", "free", None);
    assert!(t2.is_some());
    assert!(t2.unwrap().free);
}

#[test]
fn registry_resolves_auto_and_platform() {
    let r = Registry::build(
        Arc::new(Catalog::load()),
        std::env::temp_dir().join("aigw-test-data"),
    );
    use http::HeaderMap;
    let cfg = Settings::default();
    let hdr = HeaderMap::new();

    // auto → Pollinations Public
    match r.resolve("auto", b"{}", &hdr, &cfg) {
        aigw_panel::core::model::Plan::Public { model_override, .. } => {
            assert_eq!(model_override.as_deref(), Some("gpt-oss-20b"));
        }
        other => panic!("auto 应解析为 Public，实际 {:?}", other),
    }
    // @apk-doubao（原生中继）→ Native
    match r.resolve("doubao-pro@apk-doubao", b"{}", &hdr, &cfg) {
        aigw_panel::core::model::Plan::Native { pid, .. } => assert_eq!(pid, "apk-doubao"),
        other => panic!("apk-doubao 应解析为 Native，实际 {:?}", other),
    }
    // 裸名豆包模型 → Native
    match r.resolve("doubao-pro", b"{}", &hdr, &cfg) {
        aigw_panel::core::model::Plan::Native { pid, .. } => assert_eq!(pid, "apk-doubao"),
        other => panic!("doubao-pro 应解析为 Native，实际 {:?}", other),
    }
    // @pollinations（免凭据）→ Public
    match r.resolve("gpt-oss-20b@pollinations", b"{}", &hdr, &cfg) {
        aigw_panel::core::model::Plan::Public { .. } => {}
        other => panic!("pollinations 应解析为 Public，实际 {:?}", other),
    }
}

#[tokio::test]
async fn http_health_ok() {
    let state = mk_state();
    let app = aigw_panel::server::build_app(state);
    let req = axum::http::Request::builder()
        .uri("/api/health")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    let body = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
}

#[tokio::test]
async fn http_grouped_models_ok() {
    let state = mk_state();
    let app = aigw_panel::server::build_app(state);
    let req = axum::http::Request::builder()
        .uri("/api/models?action=grouped")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    let body = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
    assert!(v["total"].as_u64().unwrap_or(0) >= 100);
}

#[tokio::test]
async fn http_cross_origin_denied() {
    let state = mk_state();
    let app = aigw_panel::server::build_app(state);
    let req = axum::http::Request::builder()
        .uri("/api/overview")
        .header("origin", "https://evil.example.com")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::FORBIDDEN);
}

// ---- 2026-10-16 契约回归：拆分 routers/ 与新增 credits 后，接口形状必须锁住 ----

#[tokio::test]
async fn http_route_models_are_object_not_array() {
    // ★ 回归点：曾经 `/api/route` 把 Vec<String> 塞进 "models"，
    //   前端 Object.keys() 拿到下标 "0"/"1"，整列 undefined。
    let state = mk_state();
    let app = aigw_panel::server::build_app(state);
    let req = axum::http::Request::builder()
        .uri("/api/route")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let body = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let m = &v["models"];
    assert!(m.is_object(), "models 必须是对象映射，实际 {:?}", m);
    for (name, d) in m.as_object().unwrap() {
        assert!(
            name == "auto" || name == "free",
            "默认只应有 auto/free 两个模型，实际 {}",
            name
        );
        assert!(d["healthy"].is_number(), "{} 缺 healthy", name);
        assert!(d["total"].is_number(), "{} 缺 total", name);
        let tg = d["targets"].as_array().expect("targets 必须是数组");
        assert!(!tg.is_empty(), "{} 应有候选上游", name);
        for t in tg {
            assert!(t["name"].is_string(), "targets[].name 缺失");
            assert!(t["endpoint"].is_string(), "targets[].endpoint 缺失");
            assert!(t["state"].is_string(), "targets[].state 缺失");
        }
    }
}

#[tokio::test]
async fn http_credits_snapshot_shape() {
    // ★ 回归点：list_accounts() 会把 secret 脱敏，/api/credits 必须用 raw_rows，
    //   否则打上游全 401、包数为 0。空账号池时也必须是完整形状（前端不炸）。
    let state = mk_state();
    let app = aigw_panel::server::build_app(state);
    let req = axum::http::Request::builder()
        .uri("/api/credits")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let body = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
    for k in [
        "scanned",
        "packages",
        "live",
        "depleted",
        "total_remain",
        "at_risk",
        "dist",
        "by_platform",
        "rows",
    ] {
        assert!(v.get(k).is_some(), "credits 快照缺字段 {}", k);
    }
    assert!(v["dist"].as_array().unwrap().len() == 8, "dist 应有 8 个分桶");
    assert!(v["rows"].is_array());
}

#[tokio::test]
async fn http_nine_panels_routes_exist() {
    // 侧栏 9 个一级入口对应的后端接口都必须可达（防止拆分时漏搬某个 handler）。
    let state = mk_state();
    let app = aigw_panel::server::build_app(state);
    for uri in [
        "/api/overview",
        "/api/models?action=grouped",
        "/api/models?action=sources",
        "/api/accounts",
        "/api/credits",
        "/api/route",
        "/api/usage?range=all",
        "/api/growth",
        "/api/settings",
        "/api/login?action=platforms",
        "/api/autocheckin?action=status",
        "/api/catalog",
        "/api/checkins",
        "/api/logs",
        "/api/sites",
        "/api/custom_apis?action=list",
    ] {
        let req = axum::http::Request::builder()
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK, "{} 应 200", uri);
    }
}
