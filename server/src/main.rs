use std::collections::HashMap;

use axum::{
    body::Bytes,
    extract::{Json, MatchedPath},
    http::Method,
    routing::{get, post, put, Router},
};
use hyper::HeaderMap;
use hyper::server::Server;
use metrics_exporter_prometheus::PrometheusBuilder;
use opentelemetry::{global, KeyValue};
use opentelemetry::sdk::export::metrics::aggregation::stateless_temporality_selector;
use opentelemetry::sdk::export::trace::stdout as opentelemetry_stdout;
use opentelemetry::sdk::metrics::selectors::simple::inexpensive;
use opentelemetry::sdk::Resource;
use opentelemetry_otlp::{self, WithExportConfig};
use serde::Serialize;
use serde_json::Value;
use tower_http::classify::StatusInRangeAsFailures;
use tower_http::trace::TraceLayer;
use tracing::{info, instrument};
use tracing::level_filters::LevelFilter;
use tracing_bunyan_formatter::{BunyanFormattingLayer, JsonStorageLayer};
use tracing_subscriber::{prelude::*, Registry};

const SERVICE_NAME: &str = "echo-server";

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

    let otlp_resource = Resource::new(
        vec![
            KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                SERVICE_NAME,
            )
        ]
    );

    // this pipeline will log connection errors to stderr if it cannot reach the collector endpoint
    let otel_trace_pipeline = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint("http://localhost:4317"),
        )
        .with_trace_config(
            opentelemetry::sdk::trace::config().with_resource(
                otlp_resource.clone(),
            ),
        );
    let otel_tracer = otel_trace_pipeline
        .install_batch(opentelemetry::runtime::Tokio)
        .unwrap();
    let otel_log_trace_layer = tracing_opentelemetry::layer().with_tracer(otel_tracer);

    // let otel_metrics_controller = ;
    let otel_metrics_layer = tracing_opentelemetry::MetricsLayer::new(opentelemetry_otlp::new_pipeline()
        .metrics(
            inexpensive(),
            stateless_temporality_selector(),
            opentelemetry::runtime::Tokio,
        )
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint("http://localhost:4317"),
        )
        .with_resource(
            otlp_resource.clone(),
        )
        .build()
        .unwrap()
    );

    let subscriber = Registry::default()
        .with(file_writer_layer)
        .with(JsonStorageLayer) // stores fields across spans for the bunyan formatter
        .with(std_stream_bunyan_format_layer)
        .with(otel_log_trace_layer)
        .with(otel_metrics_layer);

    tracing::subscriber::set_global_default(subscriber).unwrap();

    // PrometheusBuilder::new().install().unwrap();

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
        ));

    info!("starting {}...", SERVICE_NAME);

    Server::bind(&"0.0.0.0:8080".parse().unwrap())
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
    info!(
        monotonic_counter.request_count = 1,
        request.endpoint = String::from(matched_path.as_str()),
        request.method = String::from(method.as_str()),
    );
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
    info!(
        monotonic_counter.request_count = 1,
        request.endpoint = String::from(matched_path.as_str()),
        request.method = String::from(method.as_str()),
    );
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

fn parse_request_headers(headers: HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
        .collect()
}
