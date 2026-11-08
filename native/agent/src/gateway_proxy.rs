use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use hermes_agent_rs::{Completions, Conversation, NoTools};
use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;

struct Gateway {
    client: Completions,
    token: String,
}

#[derive(Deserialize, serde::Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct Request {
    messages: Vec<Message>,
    stream: bool,
}

pub fn serve(address: SocketAddr, client: Completions) -> Result<(), String> {
    if !address.ip().is_loopback() {
        return Err("the Rust gateway proxy requires a loopback address".into());
    }
    let token = std::env::var("GATEWAY_PROXY_KEY")
        .ok()
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
        .ok_or("GATEWAY_PROXY_KEY is required for the Rust gateway proxy")?;
    let gateway = Arc::new(Gateway { client, token });
    let router = Router::new()
        .route("/v1/chat/completions", post(complete))
        .with_state(Arc::clone(&gateway));
    let runtime = tokio::runtime::Runtime::new().map_err(|err| err.to_string())?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|err| err.to_string())?;
        eprintln!(
            "Hermes Rust gateway proxy listening on {}",
            listener.local_addr().map_err(|err| err.to_string())?
        );
        axum::serve(listener, router)
            .await
            .map_err(|err| err.to_string())
    })
}

async fn complete(
    State(gateway): State<Arc<Gateway>>,
    headers: HeaderMap,
    Json(request): Json<Request>,
) -> Response {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !token.is_some_and(|token| bool::from(token.as_bytes().ct_eq(gateway.token.as_bytes()))) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let mut messages = request.messages;
    let Some(prompt) = messages
        .pop()
        .filter(|msg| msg.role == "user" && !msg.content.trim().is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            "the final message must be a nonempty user message",
        )
            .into_response();
    };
    if messages.iter().enumerate().any(|(i, msg)| {
        !matches!(msg.role.as_str(), "user" | "assistant") && !(i == 0 && msg.role == "system")
    }) {
        return (StatusCode::BAD_REQUEST, "unsupported gateway message role").into_response();
    }
    let answer = tokio::task::spawn_blocking(move || {
        let history: Vec<Value> = messages
            .into_iter()
            .map(|msg| json!({"role": msg.role, "content": msg.content}))
            .collect();
        Conversation::new(history, NoTools).turn(&gateway.client, &prompt.content)
    })
    .await;
    let Ok(Ok(answer)) = answer else {
        return (StatusCode::BAD_GATEWAY, "Rust agent request failed").into_response();
    };
    if request.stream {
        let chunk = json!({"choices":[{"delta":{"content":answer}}]});
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
            .into_response()
    } else {
        Json(json!({"choices":[{"message":{"role":"assistant","content":answer}}]})).into_response()
    }
}
