use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use hyper::{Body, Request, Response, Server};
use tower::make::Shared;
use tower::ServiceBuilder;

const SERVICE_NAME: &str = "echo-server";

#[path = "../../../src/lib.rs"]
mod lib;

async fn handle(_req: Request<Body>) -> Result<Response<Body>, Infallible> {
    Ok(Response::new(Body::from("Hello World")))
}

#[tokio::main]
async fn main() {
    // init our otel metrics middleware
    let otel_metrics_service_layer = lib::HTTPMetricsLayer {
        state: Arc::from(lib::HTTPMetricsLayerState::new(
            String::from(SERVICE_NAME),
            None,
        )),
    };

    let service = ServiceBuilder::new()
        .layer(otel_metrics_service_layer)
        .service_fn(handle);

    let make_service = Shared::new(service);

    let addr = SocketAddr::from(([127, 0, 0, 1], 5000));
    let server = Server::bind(&addr).serve(make_service);

    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}
