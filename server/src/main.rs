use std::collections::HashMap;

use axum::{body::Body, http::Request, Json, Router, routing::{get, post}};
use serde::Serialize;
use serde_json::{json, Value};

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(echo))
        .route("/", post(echo));

    axum::Server::bind(&"127.0.0.1:8080".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

#[derive(Serialize, Debug)]
struct EchoResponse {
    method: String,
    headers: HashMap<String, String>,
    body: String,
}

async fn echo(request: Request<Body>) -> Json<Value> {
    let (req_parts, req_body) = request.into_parts();

    let req_method = req_parts.method.to_string();

    let parsed_req_headers = req_parts.headers.iter().map(
        |(k, v)| (
            k.to_string(),
            v.to_str().unwrap_or_default().to_string()
        )
    ).collect::<HashMap<String, String>>();

    let parsed_req_body = match hyper::body::to_bytes(req_body).await {
        Ok(bytes) => match String::from_utf8(bytes.to_vec()) {
            Ok(str) => str,
            Err(_) => String::new()
        },
        Err(_) => String::new(),
    };

    let resp_body = EchoResponse {
        method: req_method,
        headers: parsed_req_headers,
        body: parsed_req_body,
    };

    Json(json!(resp_body))
}
