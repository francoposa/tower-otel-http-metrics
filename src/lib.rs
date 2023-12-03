#![doc = include_str!("../README.md")]
//! ## Examples
//! See `examples` directory in repo for runnable code and supporting config files.
//!
//! ### Hyper Server
//! Adding OpenTelementry HTTP Server metrics to a bare-bones Tower-compatible Service using [`Hyper`](https://docs.rs/crate/hyper/latest):
//!
//! ```rust
//! use std::convert::Infallible;
//! use std::net::SocketAddr;
//! use std::time::Duration;
//!
//! use hyper::{Body, Request, Response, Server};
//! use opentelemetry::sdk::resource::{
//!     EnvResourceDetector, SdkProvidedResourceDetector, TelemetryResourceDetector,
//! };
//! use opentelemetry::sdk::Resource;
//! use opentelemetry_otlp::{self, WithExportConfig};
//! use tower::make::Shared;
//! use tower::ServiceBuilder;
//!
//! use tower_otel_http_metrics;
//!
//! const SERVICE_NAME: &str = "example-tower-http-service";
//!
//! async fn handle(_req: Request<Body>) -> Result<Response<Body>, Infallible> {
//!     Ok(Response::new(Body::from("hello, world")))
//! }
//!
//! #[tokio::main]
//! async fn main() {
//!     // init otel resource config
//!     let otlp_resource_detected = Resource::from_detectors(
//!         Duration::from_secs(3),
//!         vec![
//!             Box::new(SdkProvidedResourceDetector),
//!             Box::new(EnvResourceDetector::new()),
//!             Box::new(TelemetryResourceDetector),
//!         ],
//!     );
//!     let otlp_resource_override = Resource::new(vec![
//!         opentelemetry_semantic_conventions::resource::SERVICE_NAME.string(SERVICE_NAME),
//!     ]);
//!     let otlp_resource = otlp_resource_detected.merge(&otlp_resource_override);
//!
//!     // init otel metrics pipeline
//!     // https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/#kitchen-sink-full-configuration
//!     // this configuration interface is annoyingly slightly different from the tracing one
//!     // also the above documentation is outdated, it took awhile to get this correct one working
//!     opentelemetry_otlp::new_pipeline()
//!         .metrics(opentelemetry::runtime::Tokio)
//!         .with_exporter(
//!             opentelemetry_otlp::new_exporter()
//!                 .tonic()
//!                 .with_endpoint("http://localhost:4317"),
//!         )
//!         .with_resource(otlp_resource.clone())
//!         .with_period(Duration::from_secs(15))
//!         .build() // build registers the global meter provider
//!         .unwrap();
//!
//!     // init our otel metrics middleware
//!     let otel_metrics_service_layer =
//!         tower_otel_http_metrics::HTTPMetricsLayer::new(String::from(SERVICE_NAME), None);
//
//!     let service = ServiceBuilder::new()
//!         .layer(otel_metrics_service_layer)
//!         .service_fn(handle);
//!
//!     let make_service = Shared::new(service);
//!
//!     let addr = SocketAddr::from(([127, 0, 0, 1], 5000));
//!     let server = Server::bind(&addr).serve(make_service);
//!
//!     if let Err(e) = server.await {
//!         eprintln!("server error: {}", e);
//!     }
//! }
//! ```
//!
//! [`Layer`]: tower_layer::Layer
//! [`Service`]: tower_service::Service
//! [`Future`]: tower_service::Future

use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll::Ready;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::http::{HeaderMap, Response, Version};
use axum::{extract::MatchedPath, http::Request};
use futures_util::ready;
use http_body::Body as HTTPBody;
use opentelemetry::metrics::Histogram;
use opentelemetry::KeyValue;
use opentelemetry_api::global;
use pin_project_lite::pin_project;
use tower::{Layer, Service};

const HTTP_SERVER_DURATION_METRIC: &str = "http.server.duration";

const HTTP_REQUEST_METHOD_LABEL: &str = "http.request.method";
const HTTP_ROUTE_LABEL: &str = "http.route";
const HTTP_RESPONSE_STATUS_CODE_LABEL: &str = "http.response.status_code";

const NETWORK_PROTOCOL_NAME_LABEL: &str = "network.protocol.name";
const NETWORK_PROTOCOL_VERSION_LABEL: &str = "network.protocol.version";

/// State scoped to the entire middleware Layer.
///
/// For now the only global state we hold onto is the metrics instruments.
/// The OTEL SDKs do support calling for the global meter provider instead of holding a reference
/// but it seems ideal to avoid extra access to the global meter, which sits behind a RWLock.
struct HTTPMetricsLayerState {
    pub server_request_duration: Histogram<u64>,
}

#[derive(Clone)]
/// [`Service`] used by [`HTTPMetricsLayer`]
pub struct HTTPMetricsService<S> {
    pub(crate) state: Arc<HTTPMetricsLayerState>,
    inner_service: S,
}

#[derive(Clone)]
/// [`Layer`] which applies the OTEL HTTP server metrics middleware
pub struct HTTPMetricsLayer {
    state: Arc<HTTPMetricsLayerState>,
}

impl HTTPMetricsLayer {
    // TODO convert this to a bunch of "with_whatever()" methods
    pub fn new(service_name: String, server_duration_metric_name: Option<String>) -> Self {
        let meter = global::meter(service_name);

        let mut _server_duration_metric_name = Cow::from(HTTP_SERVER_DURATION_METRIC);
        if let Some(name) = server_duration_metric_name {
            _server_duration_metric_name = name.into();
        }
        HTTPMetricsLayer {
            state: Arc::from(HTTPMetricsLayerState {
                server_request_duration: meter.u64_histogram(_server_duration_metric_name).init(),
            }),
        }
    }
}

impl<S> Layer<S> for HTTPMetricsLayer {
    type Service = HTTPMetricsService<S>;

    fn layer(&self, service: S) -> Self::Service {
        HTTPMetricsService {
            state: self.state.clone(),
            inner_service: service,
        }
    }
}

/// ResponseFutureMetricsState holds request-scoped data for metrics and their attributes.
///
/// ResponseFutureMetricsState lives inside the response future, as it needs to hold data
/// initialized or extracted from the request before it is forwarded to the inner Service.
/// The rest of the data (e.g. status code, error) can be extracted from the response
/// or calculated with respect to the data held here (e.g., duration = now - duration start).
#[derive(Clone)]
struct ResponseFutureMetricsState {
    // fields for the metrics themselves
    // http server duration: https://opentelemetry.io/docs/specs/semconv/http/http-metrics/#metric-httpserverrequestduration
    http_request_duration_start: Instant,

    // fields for metric labels
    http_request_method: String,
    http_route: String,
    network_protocol_name: String,
    network_protocol_version: String,
}

pin_project! {
    /// Response [`Future`] for [`HTTPMetricsService`].
    pub struct HTTPMetricsResponseFuture<F> {
        #[pin]
        inner_response_future: F,
        layer_state: Arc<HTTPMetricsLayerState>,
        metrics_state: ResponseFutureMetricsState,
    }
}

impl<S, R, B> Service<Request<R>> for HTTPMetricsService<S>
where
    S: Service<Request<R>, Response = Response<B>>,
    B: HTTPBody,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = HTTPMetricsResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner_service.poll_ready(cx)
    }

    fn call(&mut self, req: Request<R>) -> Self::Future {
        let duration_start = Instant::now();

        let method = req.method().as_str().to_owned();

        let mut matched_path = String::new();
        if let Some(mp) = req.extensions().get::<MatchedPath>() {
            matched_path = mp.as_str().to_owned();
        };

        // TODO get all the good stuff out of the headers
        let _headers = parse_request_headers(req.headers());

        let (protocol, version) = split_and_format_protocol_version(req.version());
        req.uri();

        HTTPMetricsResponseFuture {
            inner_response_future: self.inner_service.call(req),
            layer_state: self.state.clone(),
            metrics_state: ResponseFutureMetricsState {
                http_request_duration_start: duration_start,
                http_request_method: method,
                http_route: matched_path,
                network_protocol_name: protocol,
                network_protocol_version: version,
            },
        }
    }
}

impl<F, B: HTTPBody, E> Future for HTTPMetricsResponseFuture<F>
where
    F: Future<Output = Result<Response<B>, E>>,
{
    type Output = Result<Response<B>, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let response = ready!(this.inner_response_future.poll(cx))?;

        let labels = extract_labels_server_request_duration(this.metrics_state, &response);

        this.layer_state.server_request_duration.record(
            this.metrics_state
                .http_request_duration_start
                .elapsed()
                .as_secs(),
            &labels,
        );

        Ready(Ok(response))
    }
}

fn parse_request_headers(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
        .collect()
}

fn extract_labels_server_request_duration<T>(
    metrics_state: &ResponseFutureMetricsState,
    resp: &Response<T>,
) -> Vec<KeyValue> {
    vec![
        KeyValue::new(HTTP_ROUTE_LABEL, metrics_state.http_route.clone()),
        KeyValue::new(HTTP_RESPONSE_STATUS_CODE_LABEL, resp.status().to_string()),
        KeyValue::new(
            HTTP_REQUEST_METHOD_LABEL,
            metrics_state.http_request_method.clone(),
        ),
        KeyValue::new(
            NETWORK_PROTOCOL_NAME_LABEL,
            metrics_state.network_protocol_name.clone(),
        ),
        KeyValue::new(
            NETWORK_PROTOCOL_VERSION_LABEL,
            metrics_state.network_protocol_version.clone(),
        ),
    ]
}

fn split_and_format_protocol_version(http_version: Version) -> (String, String) {
    let string_http_version = format!("{:?}", http_version);
    let mut split = string_http_version.split("/");
    let mut scheme = String::new();
    if let Some(next) = split.next() {
        scheme = next.to_owned().to_lowercase();
    };
    let mut version = String::new();
    if let Some(next) = split.next() {
        version = next.to_owned();
    };
    return (scheme, version);
}
