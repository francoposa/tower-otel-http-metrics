use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Bytes,
    extract::{Json, MatchedPath},
    http::Method,
    routing::{get, post, put, Router},
};
use echo_server_logging_metrics_tracing as lib;
use hyper::server::Server;
use hyper::HeaderMap;
use opentelemetry::sdk::resource::{
    EnvResourceDetector, SdkProvidedResourceDetector, TelemetryResourceDetector,
};
use opentelemetry::sdk::Resource;
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::{self, WithExportConfig};
use serde::Serialize;
use serde_json::Value;
use tower_http::classify::StatusInRangeAsFailures;
use tower_http::trace::TraceLayer;
use tracing::level_filters::LevelFilter;
use tracing::{info, instrument};
use tracing_bunyan_formatter::{BunyanFormattingLayer, JsonStorageLayer};
use tracing_subscriber::{prelude::*, Registry};

const SERVICE_NAME: &str = "echo-server";

// attempting to adhere to not-yet-finalized semantic conventions for OTEL metrics:
// https://opentelemetry.io/docs/specs/otel/metrics/semantic_conventions/#pluralization
const HTTP_SERVER_REQUEST_COUNT_METRIC: &str = "http.server.request.count";

#[tokio::main]
async fn main() {
    // file writer layer to collect all levels of logs, mostly useful for debugging the logging setup
    let file_appender = tracing_appender::rolling::minutely("./logs", "trace");
    let (file_writer, _guard) = tracing_appender::non_blocking(file_appender);
    let file_writer_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(file_writer);

    // stdout/stderr log layer for non-tracing logs to be collected into ElasticSearch or similar
    let std_stream_bunyan_format_layer =
        BunyanFormattingLayer::new(SERVICE_NAME.into(), std::io::stderr)
            .with_filter(LevelFilter::INFO);

    //
    // opentelemetry-formatted tracing layer to send traces to collector
    //

    // see more about opentelemetry propagators here:
    // https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/context/api-propagators.md
    global::set_text_map_propagator(opentelemetry::sdk::propagation::TraceContextPropagator::new());

    // use this stdout pipeline instead to debug or view the opentelemetry data without a collector
    // let otel_tracer = opentelemetry_stdout::new_pipeline().install_simple();

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

    // init otel tracing pipeline
    // https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/#kitchen-sink-full-configuration
    // this pipeline will log connection errors to stderr if it cannot reach the collector endpoint
    let otel_trace_pipeline = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint("http://localhost:4317"),
        )
        .with_trace_config(
            opentelemetry::sdk::trace::config().with_resource(otlp_resource.clone()),
        );
    let otel_tracer = otel_trace_pipeline
        .install_batch(opentelemetry::runtime::Tokio)
        .unwrap();
    let otel_log_trace_layer = tracing_opentelemetry::layer().with_tracer(otel_tracer);

    // init otel metrics pipeline
    // https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/#kitchen-sink-full-configuration
    // this configuration interface is annoyingly slightly different from the tracing one
    let otel_metrics_layer = tracing_opentelemetry::MetricsLayer::new(
        opentelemetry_otlp::new_pipeline()
            .metrics(opentelemetry::runtime::Tokio)
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_endpoint("http://localhost:4317"),
            )
            .with_resource(otlp_resource.clone())
            .build()
            .unwrap(),
    );

    let subscriber = Registry::default()
        .with(file_writer_layer)
        .with(JsonStorageLayer) // stores fields across spans for the bunyan formatter
        .with(std_stream_bunyan_format_layer)
        .with(otel_log_trace_layer)
        .with(otel_metrics_layer);

    tracing::subscriber::set_global_default(subscriber).unwrap();

    let app = Router::new()
        .route("/", get(echo))
        .route("/", post(echo))
        .route("/", put(echo))
        .route("/json", get(echo_json))
        .route("/json", post(echo_json))
        .route("/json", put(echo_json))
        .layer(TraceLayer::new(
            // by default the tower http trace layer only classifies 5xx errors as failures
            StatusInRangeAsFailures::new(400..=599).into_make_classifier(),
        ))
        .layer(lib::HTTPMetricsLayer {
            state: Arc::from(echo_server_logging_metrics_tracing::HTTPMetricsLayerState {
                server_request_count: global::meter(SERVICE_NAME)
                    .u64_counter(HTTP_SERVER_REQUEST_COUNT_METRIC)
                    .init(),
            }),
        });

    info!("starting {}...", SERVICE_NAME);

    Server::bind(&"0.0.0.0:5000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

#[instrument(skip(headers, bytes))]
pub async fn echo(
    matched_path: MatchedPath,
    method: Method,
    headers: HeaderMap,
    bytes: Bytes,
) -> Bytes {
    // ideally this would be in some middleware instead of manually in each handler
    let labels = [
        KeyValue::new("endpoint", String::from(matched_path.as_str())),
        KeyValue::new("method", String::from(method.as_str())),
    ];
    increment_u64_counter(HTTP_SERVER_REQUEST_COUNT_METRIC, 1, &labels);

    let parsed_req_headers = parse_request_headers(headers);
    // method and headers get logged by the instrument macro; this is just an example
    info!(
        request.endpoint = String::from(matched_path.as_str()),
        request.method = %method,
        request.headers = ?parsed_req_headers,
        "parsed request headers",
    );
    bytes
}

#[derive(Serialize, Debug)]
struct EchoJSONResponse {
    method: String,
    headers: HashMap<String, String>,
    body: Value,
}

#[instrument(skip(headers, body))]
async fn echo_json(
    matched_path: MatchedPath,
    method: Method,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<EchoJSONResponse> {
    // ideally this would be in some middleware instead of manually in each handler
    let labels = [
        KeyValue::new("endpoint", String::from(matched_path.as_str())),
        KeyValue::new("method", String::from(method.as_str())),
    ];
    increment_u64_counter(HTTP_SERVER_REQUEST_COUNT_METRIC, 1, &labels);

    let req_method = method.to_string();
    let parsed_req_headers = parse_request_headers(headers);
    // method and headers get logged by the instrument macro; this is just an example
    info!(
        request.endpoint = String::from(matched_path.as_str()),
        request.method = req_method,
        request.headers = ?parsed_req_headers,
        "parsed request headers",
    );

    let resp_body = EchoJSONResponse {
        method: req_method,
        headers: parsed_req_headers,
        body,
    };

    Json(resp_body)
}

fn increment_u64_counter(name: impl Into<Cow<'static, str>>, value: u64, labels: &[KeyValue]) {
    // let meter = global::meter(SERVICE_NAME);
    // let counter = meter.u64_counter(name).init();
    // counter.add(value, labels);
}

fn parse_request_headers(headers: HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
        .collect()
}
