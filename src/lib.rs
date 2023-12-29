#![doc = include_str!("../README.md")]
//!
//! [`Layer`]: tower_layer::Layer
//! [`Service`]: tower_service::Service
//! [`Future`]: tower_service::Future

use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::string::String;
use std::sync::Arc;
use std::task::Poll::Ready;
use std::task::{Context, Poll};
use std::time::Instant;

#[cfg(feature = "axum")]
use axum::extract::MatchedPath;
use futures_util::ready;
use http::{Response, Version};
use http_body::Body as HTTPBody;
use opentelemetry_api::global;
use opentelemetry_api::metrics::Histogram;
use opentelemetry_api::KeyValue;
use pin_project_lite::pin_project;
use tower_layer::Layer;
use tower_service::Service;

const HTTP_SERVER_DURATION_METRIC: &str = "http.server.request.duration";

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
    pub fn new(service_name: String) -> Self {
        let meter = global::meter(service_name);
        HTTPMetricsLayer {
            state: Arc::from(HTTPMetricsLayerState {
                server_request_duration: meter
                    .u64_histogram(Cow::from(HTTP_SERVER_DURATION_METRIC))
                    .init(),
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

impl<ReqBody, ResBody, S> Service<http::Request<ReqBody>> for HTTPMetricsService<S>
where
    S: Service<http::Request<ReqBody>, Response = http::Response<ResBody>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = HTTPMetricsResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner_service.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        let duration_start = Instant::now();

        let method = req.method().as_str().to_owned();

        #[allow(unused_mut)]
        let mut matched_path = String::new();
        #[cfg(feature = "axum")]
        if let Some(mp) = req.extensions().get::<MatchedPath>() {
            matched_path = mp.as_str().to_owned();
        };

        // TODO get all the good stuff out of the headers
        // let _headers = parse_request_headers(req.headers());

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

impl<F, ResBody, E> Future for HTTPMetricsResponseFuture<F>
where
    F: Future<Output = Result<http::Response<ResBody>, E>>,
{
    type Output = F::Output;

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
    let version_str = match http_version {
        http::Version::HTTP_09 => "0.9",
        http::Version::HTTP_10 => "1.0",
        http::Version::HTTP_11 => "1.1",
        http::Version::HTTP_2 => "2.0",
        http::Version::HTTP_3 => "3.0",
        _ => "",
    };
    (String::from("http"), String::from(version_str))
}
