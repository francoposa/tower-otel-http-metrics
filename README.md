# echo-server-rust-logging-tracing-metrics

Compiling requires the protobuf compiler packages, usually called `protobuf-devel` or similar in Linux repos

The easiest way to view traces is with the Jaeger all-in-one docker image.
Jaeger added support for OpenTelemetry-formatted traces in v1.35

```shell
docker run -d --name jaeger \
  -e COLLECTOR_OTLP_ENABLED=true \
  -p 16686:16686 \
  -p 4317:4317 \
  -p 4318:4318 \
  jaegertracing/all-in-one:1.35
```

Then access the Jaeger UI at http://localhost:16686.
The echo server service will appear once traces have been produced - run a curl command to create activity on the
server:

```shell
curl -i -X GET --header "content-type: application/json" localhost:8080/json -d '{"hello": "world"}'
```

Trace spans do not link together much at this point - I believe this is due to the lack of tracing support thus far in
Hyper.