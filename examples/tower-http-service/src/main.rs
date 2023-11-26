use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hyper::{Body, Request, Response, Server};
use opentelemetry::sdk::resource::{
    EnvResourceDetector, SdkProvidedResourceDetector, TelemetryResourceDetector,
};
use opentelemetry::sdk::Resource;
use opentelemetry_otlp::{self, WithExportConfig};
use tower::make::Shared;
use tower::ServiceBuilder;

const SERVICE_NAME: &str = "example-tower-http-servicer";

#[path = "../../../src/lib.rs"]
mod lib;

async fn handle(_req: Request<Body>) -> Result<Response<Body>, Infallible> {
    Ok(Response::new(Body::from("hello, world")))
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
        .metrics(opentelemetry::runtime::Tokio)
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
