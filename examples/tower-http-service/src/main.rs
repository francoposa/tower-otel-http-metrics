use std::borrow::Cow;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::{Request, Response};
use opentelemetry_api::global;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_otlp::{self};
use opentelemetry_sdk::resource::{
    EnvResourceDetector, SdkProvidedResourceDetector, TelemetryResourceDetector,
};
use opentelemetry_sdk::Resource;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_otel_http_metrics;

const SERVICE_NAME: &str = "example-tower-http-service";

async fn handle(_req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
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
