use std::collections::HashMap;

use axum::{
    body::Bytes,
    extract::Json,
    http::Method,
    routing::{get, post, put, Router},
};
use hyper::server::Server;
use hyper::HeaderMap;
use opentelemetry::global;
use opentelemetry::runtime::Tokio;
use opentelemetry::sdk::export::trace::stdout as opentelemetry_stdout;
use serde::Serialize;
use serde_json::Value;
use tower_http::classify::StatusInRangeAsFailures;
use tower_http::trace::TraceLayer;
use tracing::level_filters::LevelFilter;
use tracing::{info, instrument};
use tracing_bunyan_formatter::{BunyanFormattingLayer, JsonStorageLayer};
use tracing_subscriber::{prelude::*, Registry};

const SERVICE_NAME: &str = "axum-echo-server-logging-tracing";

#[tokio::main]
async fn main() {
    // file writer layer to collect all levels of logs, mostly useful for debugging the logging setup
    let file_appender = tracing_appender::rolling::minutely("./logs", "trace");
    let (file_writer, _guard) = tracing_appender::non_blocking(file_appender);
    let file_writer_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(file_writer);

    // opentelemetry-formatted tracing layer to send traces to jaeger/jaeger-compatible collector
    // see more about opentelemetry propagators here:
    // https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/context/api-propagators.md
    global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());
    // opentelemetry pipeline for sending to jaeger collector; port matches default jaeger docker setup
    // this pipeline will just log connection errors to stderr if it cannot reach the collector endpoint
    let tracer = opentelemetry_jaeger::new_agent_pipeline()
        .with_service_name(SERVICE_NAME)
        .with_endpoint("localhost:6831")
        .with_auto_split_batch(true)
        .install_batch(Tokio)
        .unwrap();
    // use this stdout pipeline instead to debug or view the opentelemetry data without a collector
    // let tracer = opentelemetry_stdout::new_pipeline().install_simple();
    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

    // stdout/stderr log layer for non-tracing logs to be collected into ElasticSearch or similar
    let std_stream_bunyan_format_layer =
        BunyanFormattingLayer::new(SERVICE_NAME.into(), std::io::stdout)
            .with_filter(LevelFilter::DEBUG);

    let subscriber = Registry::default()
        .with(file_writer_layer)
        .with(JsonStorageLayer)
        .with(std_stream_bunyan_format_layer)
        .with(telemetry);

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
        ));

    Server::bind(&"127.0.0.1:8080".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

#[instrument(skip(headers, bytes), fields(req.body.len = bytes.len()))]
pub async fn echo(method: Method, headers: HeaderMap, bytes: Bytes) -> Bytes {
    let parsed_req_headers = parse_request_headers(headers);
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
    method: Method,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<EchoJSONResponse> {
    let req_method = method.to_string();
    let parsed_req_headers = parse_request_headers(headers);
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
