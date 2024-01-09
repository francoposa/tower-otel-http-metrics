# Tower OTEL Metrics Middleware

OpenTelemetry Metrics Middleware for Tower-compatible Rust HTTP servers.

## Examples

See `examples` directory in repo for runnable code and supporting config files.
Attempts are made to keep the code here synced, but it will not be perfect.

OTEL libraries in particular are sensitive to minor version changes at this point,
so the examples may only work with the OTEL crate versions pinned in `examples`.

### Axum Server

Adding OpenTelementry HTTP Server metrics using the [`Axum`](https://docs.rs/axum/latest/axum) framework
over a Tower-compatible [`Hyper`](https://docs.rs/hyper/latest/hyper) Service:

```rust
use std::borrow::Cow;
use std::time::Duration;

use axum::routing::{get, post, put, Router};
use bytes::Bytes;
use opentelemetry_api::global;
use opentelemetry_otlp::{
    WithExportConfig, {self},
};
use opentelemetry_sdk::resource::{
    EnvResourceDetector, SdkProvidedResourceDetector, TelemetryResourceDetector,
};
use opentelemetry_sdk::Resource;
use tower_otel_http_metrics;

const SERVICE_NAME: &str = "example-axum-http-service";

fn init_otel_resource() -> Resource {
    let otlp_resource_detected = Resource::from_detectors(
        Duration::from_secs(3),
        vec![
            Box::new(SdkProvidedResourceDetector),
            Box::new(EnvResourceDetector::new()),
            Box::new(TelemetryResourceDetector),
        ],
    );
    let otlp_resource_override = Resource::new(vec![
        opentelemetry_semantic_conventions::resource::SERVICE_NAME.string(SERVICE_NAME),
    ]);
    otlp_resource_detected.merge(&otlp_resource_override)
}

async fn handle() -> Bytes {
    Bytes::from("hello, world")
}

#[tokio::main]
async fn main() {
    // init otel metrics pipeline
    // https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/#kitchen-sink-full-configuration
    // this configuration interface is annoyingly slightly different from the tracing one
    // also the above documentation is outdated, it took awhile to get this correct one working
    opentelemetry_otlp::new_pipeline()
        .metrics(opentelemetry_sdk::runtime::Tokio)
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint("http://localhost:4317"),
        )
        .with_resource(init_otel_resource().clone())
        .with_period(Duration::from_secs(10))
        .build() // build registers the global meter provider
        .unwrap();

    // init our otel metrics middleware
    let global_meter = global::meter(Cow::from(SERVICE_NAME));
    let otel_metrics_service_layer = tower_otel_http_metrics::HTTPMetricsLayerBuilder::new()
        .with_meter(global_meter)
        .build()
        .unwrap();

    let app = Router::new()
        .route("/", get(handle))
        .route("/", post(handle))
        .route("/", put(handle))
        .layer(otel_metrics_service_layer);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:5000").await.unwrap();
    let server = axum::serve(listener, app);

    if let Err(err) = server.await {
        eprintln!("server error: {}", err);
    }
}
```

### Hyper Server

Adding OpenTelementry HTTP Server metrics to a bare-bones Tower-compatible Service
using [`Hyper`](https://docs.rs/crate/hyper/latest):

```rust
use std::borrow::Cow;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::{Request, Response};
use opentelemetry_api::global;
use opentelemetry_otlp::{
    WithExportConfig, {self},
};
use opentelemetry_sdk::resource::{
    EnvResourceDetector, SdkProvidedResourceDetector, TelemetryResourceDetector,
};
use opentelemetry_sdk::Resource;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_otel_http_metrics;

const SERVICE_NAME: &str = "example-tower-http-service";

fn init_otel_resource() -> Resource {
    let otlp_resource_detected = Resource::from_detectors(
        Duration::from_secs(3),
        vec![
            Box::new(SdkProvidedResourceDetector),
            Box::new(EnvResourceDetector::new()),
            Box::new(TelemetryResourceDetector),
        ],
    );
    let otlp_resource_override = Resource::new(vec![
        opentelemetry_semantic_conventions::resource::SERVICE_NAME.string(SERVICE_NAME),
    ]);
    otlp_resource_detected.merge(&otlp_resource_override)
}

async fn handle(_req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from("hello, world"))))
}

#[tokio::main]
async fn main() {
    // init otel metrics pipeline
    // https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/#kitchen-sink-full-configuration
    // this configuration interface is annoyingly slightly different from the tracing one
    // also the above documentation is outdated, it took awhile to get this correct one working
    opentelemetry_otlp::new_pipeline()
        .metrics(opentelemetry_sdk::runtime::Tokio)
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint("http://localhost:4317"),
        )
        .with_resource(init_otel_resource().clone())
        .with_period(Duration::from_secs(10))
        .build() // build registers the global meter provider
        .unwrap();

    // init our otel metrics middleware
    let global_meter = global::meter(Cow::from(SERVICE_NAME));
    let otel_metrics_service_layer = tower_otel_http_metrics::HTTPMetricsLayerBuilder::new()
        .with_meter(global_meter)
        .build()
        .unwrap();

    let tower_service = ServiceBuilder::new()
        .layer(otel_metrics_service_layer)
        .service_fn(handle);
    let hyper_service = hyper_util::service::TowerToHyperService::new(tower_service);

    let addr = SocketAddr::from(([127, 0, 0, 1], 5000));
    let listener = TcpListener::bind(addr).await.unwrap();

    loop {
        let (stream, _) = listener.accept().await.unwrap();

        let io = hyper_util::rt::TokioIo::new(stream);
        let service_clone = hyper_service.clone();

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(io, service_clone)
                .await
            {
                eprintln!("server error: {}", err);
            }
        });
    }
}
```
