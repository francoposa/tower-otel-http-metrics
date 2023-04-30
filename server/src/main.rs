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
use opentelemetry::sdk::export::trace::stdout as opentelemetry_stdout;
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

const REQUEST_COUNT_METRIC_NAME: &str = "request_count";
const METRIC_LABEL_SERVICE: &str = "service";
const METRIC_LABEL_REQUEST_ENDPOINT: &str = "endpoint";
const METRIC_LABEL_REQUEST_METHOD: &str = "method";

#[tokio::main]
async fn main() {
    // file writer layer to collect all levels of logs, mostly useful for debugging the logging setup
    let file_appender = tracing_appender::rolling::minutely("./logs", "trace");
    let (file_writer, _guard) = tracing_appender::non_blocking(file_appender);
    let file_writer_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(file_writer);

    //
    // opentelemetry-formatted tracing layer to send traces to collector
    //

    // see more about opentelemetry propagators here:
    // https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/context/api-propagators.md
    global::set_text_map_propagator(opentelemetry::sdk::propagation::TraceContextPropagator::new());

    // use this stdout pipeline instead to debug or view the opentelemetry data without a collector
    // let otel_tracer = opentelemetry_stdout::new_pipeline().install_simple();

    // this pipeline will log connection errors to stderr if it cannot reach the collector endpoint
    let otel_pipeline = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint("http://localhost:4317"),
        )
        .with_trace_config(
            opentelemetry::sdk::trace::config().with_resource(
                Resource::new(
                    vec![
                        KeyValue::new(
                            opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                            SERVICE_NAME,
                        )
                    ]
                )
            ),
        );
    let otel_tracer = otel_pipeline
        .install_batch(opentelemetry::runtime::Tokio)
        .unwrap();
    let otel_layer = tracing_opentelemetry::layer().with_tracer(otel_tracer);

    // stdout/stderr log layer for non-tracing logs to be collected into ElasticSearch or similar
    let std_stream_bunyan_format_layer =
        BunyanFormattingLayer::new(SERVICE_NAME.into(), std::io::stderr)
            .with_filter(LevelFilter::INFO);

    let subscriber = Registry::default()
        .with(file_writer_layer)
        .with(JsonStorageLayer) // stores fields across spans for the bunyan formatter
        .with(std_stream_bunyan_format_layer)
        .with(otel_layer);

    tracing::subscriber::set_global_default(subscriber).unwrap();

    // let meter = opentelemetry_otlp::new_pipeline()
    //     .metrics(
    //         inexpensive(),
    //         stateless_temporality_selector(),
    //         opentelemetry::runtime::Tokio,
    //     )
    //     .with_exporter(
    //         opentelemetry_otlp::new_exporter()
    //             .tonic()
    //             .with_endpoint("http://localhost:4317"),
    //     )
    //     .build()
    //     .unwrap();

    PrometheusBuilder::new().install().unwrap();

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

#[instrument(skip(headers, bytes), fields(req.body.len = bytes.len()))]
pub async fn echo(
    matched_path: MatchedPath,
    method: Method,
    headers: HeaderMap,
    bytes: Bytes,
) -> Bytes {
    metrics::increment_counter!(
        String::from(REQUEST_COUNT_METRIC_NAME),
        METRIC_LABEL_SERVICE => SERVICE_NAME,
        METRIC_LABEL_REQUEST_ENDPOINT => String::from(matched_path.as_str()),
        METRIC_LABEL_REQUEST_METHOD => String::from(method.as_str()),
    );
    let parsed_req_headers = parse_request_headers(headers);
    // method and headers get logged by the instrument macro; this is just an example
    info!(
        req.method = %method,
        req.headers = ?parsed_req_headers,
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

#[instrument(skip(headers, body), fields(req.headers.content_length = headers.len()))]
async fn echo_json(
    matched_path: MatchedPath,
    method: Method,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<EchoJSONResponse> {
    metrics::increment_counter!(
        String::from(REQUEST_COUNT_METRIC_NAME),
        METRIC_LABEL_SERVICE => SERVICE_NAME,
        METRIC_LABEL_REQUEST_ENDPOINT => String::from(matched_path.as_str()),
        METRIC_LABEL_REQUEST_METHOD => String::from(method.as_str()),
    );
    let req_method = method.to_string();
    let parsed_req_headers = parse_request_headers(headers);
    // method and headers get logged by the instrument macro; this is just an example
    info!(
        req.method = req_method,
        req.headers = ?parsed_req_headers,
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
