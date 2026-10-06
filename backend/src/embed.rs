use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG};
use axum::http::{HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

use crate::AppState;

/// 内嵌前端构建产物（frontend/dist），实现单二进制部署。
/// 发布前需先执行 `npm run build` 生成 dist 目录。
#[derive(Embed)]
#[folder = "../frontend/dist"]
pub struct Asset;

/// 把 `<base href="{prefix}/">` 注入 index.html 的 `<head>` 开头。
///
/// 构建产物用相对路径引用资源（vite `base: './'`），浏览器按 base 解析，
/// 于是资源、动态 import 与接口请求都会自动带上安全入口前缀；未启用入口时
/// base 为 `/`，行为与不带 base 标签时一致。
fn inject_base(html: &str, entrance: &str) -> String {
    let href = if entrance.is_empty() {
        "/".to_string()
    } else {
        format!("/{entrance}/")
    };
    let tag = format!("<base href=\"{href}\">");
    match html.find("<head>") {
        Some(i) => {
            let at = i + "<head>".len();
            format!("{}{}{}", &html[..at], tag, &html[at..])
        }
        None => format!("{tag}{html}"),
    }
}

/// 静态资源服务：命中文件则返回，否则回退到 index.html（SPA 前端路由）
/// 缓存策略：/assets/* 文件名带内容哈希，可长期强缓存；
/// index.html 不缓存，保证发版后浏览器立即拿到新资源引用。
pub async fn handler(State(state): State<AppState>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if !path.is_empty()
        && let Some(file) = Asset::get(path) {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let cache = if path.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            return (
                [
                    (
                        CONTENT_TYPE,
                        HeaderValue::from_str(mime.as_ref()).unwrap_or_else(|_| {
                            HeaderValue::from_static("application/octet-stream")
                        }),
                    ),
                    (CACHE_CONTROL, HeaderValue::from_static(cache)),
                ],
                file.data,
            )
                .into_response();
        }
    match Asset::get("index.html") {
        Some(file) => {
            let entrance = state
                .db
                .get_setting_async(crate::security::KEY_ENTRANCE)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            let entrance = entrance.trim().trim_matches('/');
            let body = inject_base(&String::from_utf8_lossy(&file.data), entrance);
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

#[cfg(test)]
mod tests {
    use super::inject_base;

    #[test]
    fn injects_base_at_head_start() {
        let html = "<!doctype html><html><head><title>t</title></head></html>";
        let out = inject_base(html, "secret1");
        assert!(out.contains("<head><base href=\"/secret1/\">"));
        // 未启用入口时退回根路径，行为与不带 base 等价
        let out = inject_base(html, "");
        assert!(out.contains("<head><base href=\"/\">"));
    }

    #[test]
    fn prepends_when_head_missing() {
        let out = inject_base("<html></html>", "abc123");
        assert!(out.starts_with("<base href=\"/abc123/\">"));
    }
}

