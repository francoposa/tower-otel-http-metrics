use axum::{
    body::Body,
    http::{header::HeaderMap, Request},
    response::IntoResponse,
    routing::get,
    Router,
};

#[tokio::main]
async fn main() {
    let app = Router::new().route("/", get(hello));

    axum::Server::bind(&"127.0.0.1:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn hello(request: Request<Body>) -> impl IntoResponse {
    let x = request.headers();
    let y = request.body();
    println!("{:?}", x);
    println!("{:?}", y);
    "hello, world"
}
