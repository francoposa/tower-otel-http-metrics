use std::collections::HashMap;

use axum::{
    body::Body,
    http::Request,
    routing::{get, post},
    Json, Router,
};
use hyper::server::Server;
use opentelemetry::global;
use opentelemetry::runtime::Tokio;
use opentelemetry::sdk::export::trace::stdout as opentelemetry_stdout;
use serde::Serialize;
use serde_json::{json, Value};
use tower_http::classify::StatusInRangeAsFailures;
use tower_http::trace::TraceLayer;
use tracing::level_filters::LevelFilter;
use tracing::{info, instrument};
use tracing_bunyan_formatter::BunyanFormattingLayer;
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
        .with(std_stream_bunyan_format_layer)
        .with(telemetry);

    tracing::subscriber::set_global_default(subscriber).unwrap();

    let app = Router::new()
        .route("/", get(echo))
        .route("/", post(echo))
        .layer(TraceLayer::new(
            // by default the tower http trace layer only classifies 5xx errors as failures
            StatusInRangeAsFailures::new(400..=599).into_make_classifier(),
        ));

    Server::bind(&"127.0.0.1:8080".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

#[derive(Serialize, Debug)]
struct EchoResponse {
    method: String,
    headers: HashMap<String, String>,
    body: String,
}

#[instrument]
async fn echo(request: Request<Body>) -> Json<Value> {
    let (req_parts, req_body) = request.into_parts();

    let req_method = req_parts.method.to_string();

    let parsed_req_headers = req_parts
        .headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
        .collect::<HashMap<String, String>>();

    let parsed_req_body = match hyper::body::to_bytes(req_body).await {
        Ok(bytes) => match String::from_utf8(bytes.to_vec()) {
            Ok(str) => str,
            Err(_) => String::new(),
        },
        Err(_) => String::new(),
    };

    // example of info log in instrumented fn with KV fields; key name inferred from variable name
    info!(parsed_req_body, "parsed request body");

    let resp_body = EchoResponse {
        method: req_method,
        headers: parsed_req_headers,
        body: parsed_req_body,
    };

    Json(json!(resp_body))
}
