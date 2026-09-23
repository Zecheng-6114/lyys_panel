use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG};
use axum::http::{HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

/// 内嵌前端构建产物（frontend/dist），实现单二进制部署。
/// 发布前需先执行 `npm run build` 生成 dist 目录。
#[derive(Embed)]
#[folder = "../frontend/dist"]
pub struct Asset;

/// 静态资源服务：命中文件则返回，否则回退到 index.html（SPA 前端路由）
/// 缓存策略：/assets/* 文件名带内容哈希，可长期强缓存；
/// index.html 不缓存，保证发版后浏览器立即拿到新资源引用。
pub async fn handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if !path.is_empty() {
        if let Some(file) = Asset::get(path) {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let cache = if path.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            return (
                [
                    (CONTENT_TYPE, HeaderValue::from_str(mime.as_ref()).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))),
                    (CACHE_CONTROL, HeaderValue::from_static(cache)),
                ],
                file.data,
            )
                .into_response();
        }
    }
    match Asset::get("index.html") {
        Some(file) => {
            let body = String::from_utf8_lossy(&file.data).into_owned();
            let etag = format!("\"{:x}\"", {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                body.hash(&mut h);
                h.finish()
            });
            Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "text/html; charset=utf-8")
                .header(CACHE_CONTROL, "no-cache")
                .header(ETAG, etag)
                .body(body.into())
                .unwrap()
        }
        None => (
            StatusCode::NOT_FOUND,
            "前端未构建：请先在 frontend/ 执行 npm run build",
        )
            .into_response(),
    }
}
