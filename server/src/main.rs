use std::collections::HashMap;
use axum::{
    body::Body,
    http::{Request},
    response::Json,
    routing::get,
    Router,
};
use serde_json::{Value, json};

#[tokio::main]
async fn main() {
    let app = Router::new().route("/", get(echo));

    axum::Server::bind(&"127.0.0.1:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn echo(request: Request<Body>) -> Json<Value> {
    let (req_parts, req_body) = request.into_parts();
    let req_method = req_parts.method.as_str();
    let req_headers = req_parts.headers;

    // let req_host = match req_headers.get("Host") {
    //     None => "",
    //     Some(method) => method.to_str().unwrap(),
    // };

    // let mut resp_body = HashMap::new();
    // resp_body.insert("method".to_string(), req_method);

    println!("{:?}", req_headers);
    println!("{:?}", req_body);
    Json(json!({}))
}
