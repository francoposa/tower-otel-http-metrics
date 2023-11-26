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

/// HTTPMetricsLayerState holds state global to the entire Service layer.
///
/// For now the only global state we hold onto is the metrics instruments.
/// The OTEL SDKs do support calling for the global meter provider instead of holding a reference
/// but it seems ideal to avoid extra access to the global meter, which sits behind a RWLock.
pub struct HTTPMetricsLayerState {
    pub server_request_duration: Histogram<u64>,
}

impl HTTPMetricsLayerState {
    // TODO convert this to a bunch of "with_whatever()" methods
    pub fn new(service_name: String, server_duration_metric_name: Option<String>) -> Self {
        let meter = global::meter(service_name);

        let mut _server_duration_metric_name = Cow::from(HTTP_SERVER_DURATION_METRIC);
        if let Some(name) = server_duration_metric_name {
            _server_duration_metric_name = name.into();
        }

        HTTPMetricsLayerState {
            server_request_duration: meter.u64_histogram(_server_duration_metric_name).init(),
        }
    }
}

#[derive(Clone)]
pub struct HttpMetricsService<S> {
    pub(crate) state: Arc<HTTPMetricsLayerState>,
    inner_service: S,
}

#[derive(Clone)]
pub struct HTTPMetricsLayer {
    pub state: Arc<HTTPMetricsLayerState>,
}

impl<S> Layer<S> for HTTPMetricsLayer {
    type Service = HttpMetricsService<S>;

    fn layer(&self, service: S) -> Self::Service {
        HttpMetricsService {
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
    #[project = ResponseFutureProj]
    pub struct ResponseFuture<F> {
        #[pin]
        inner_response_future: F,
        layer_state: Arc<HTTPMetricsLayerState>,
        metrics_state: ResponseFutureMetricsState,
    }
}

impl<S, R, B> Service<Request<R>> for HttpMetricsService<S>
where
    S: Service<Request<R>, Response = Response<B>>,
    B: HTTPBody,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future>;

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
        let headers = parse_request_headers(req.headers());

        let (protocol, version) = split_and_format_protocol_version(req.version());
        req.uri();

        ResponseFuture {
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

impl<F, B: HTTPBody, E> Future for ResponseFuture<F>
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
