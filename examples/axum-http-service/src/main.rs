use axum::routing::{get, post, put, Router};
use bytes::Bytes;
use opentelemetry::global;
use opentelemetry_otlp::{
    WithExportConfig, {self},
};
use opentelemetry_sdk::Resource;
use tower_otel_http_metrics;

const SERVICE_NAME: &str = "example-axum-http-service";

fn init_otel_resource() -> Resource {
    Resource::builder().with_service_name(SERVICE_NAME).build()
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

    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint("http://localhost:4317")
        .build()
        .unwrap();

    let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_periodic_exporter(exporter)
        .with_resource(init_otel_resource())
        .build();

    global::set_meter_provider(meter_provider);
    // init our otel metrics middleware
    let global_meter = global::meter(SERVICE_NAME);
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
