NAME   := ghcr.io/francoposa/echo-server-rust-logging-metrics-tracing/echo-server
TAG    := $$(git rev-parse --short HEAD)
IMG    := ${NAME}:${TAG}
LATEST := ${NAME}:latest

build:
	docker build -t ${IMG} .
	docker tag ${IMG} ${LATEST}
.PHONY: build

push:
	docker push ${NAME}
.PHONY: push

