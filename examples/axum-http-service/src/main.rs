use std::convert::Infallible;
use std::time::Duration;

use axum::routing::{get, post, put, Router};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_otlp::{self};
use opentelemetry_sdk::resource::{
    EnvResourceDetector, SdkProvidedResourceDetector, TelemetryResourceDetector,
};
use opentelemetry_sdk::Resource;
use tower::ServiceBuilder;

use tower_otel_http_metrics;
use tower_otel_http_metrics::MyLayer;

const SERVICE_NAME: &str = "example-axum-http-service";

async fn handle(_: Request<impl hyper::body::Body>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(Bytes::from("hello, world"))))
}

#[tokio::main]
async fn main() {
    // init otel resource config
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
    let otlp_resource = otlp_resource_detected.merge(&otlp_resource_override);

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
        .with_resource(otlp_resource.clone())
        .with_period(Duration::from_secs(15))
        .build() // build registers the global meter provider
        .unwrap();

    // init our otel metrics middleware
    // let otel_metrics_service_layer =
    //     tower_otel_http_metrics::HTTPMetricsLayer::new(String::from(SERVICE_NAME));

    let app = Router::new()
        .route("/", get(handle))
        .route("/", post(handle))
        .route("/", put(handle))
        .layer(ServiceBuilder::new().layer(MyLayer));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:5000").await.unwrap();
    let server = axum::serve(listener, app);

    // info!("starting {}...", SERVICE_NAME);

    // let server = Server::bind(&"0.0.0.0:5000".parse().unwrap()).serve(app.into_make_service());

    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}
