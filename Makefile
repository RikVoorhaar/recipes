.PHONY: ci fmt clippy test test-python container-build

# Keep CI and local runs on the same smaller debug profiles.
export CARGO_PROFILE_DEV_DEBUG ?= 0
export CARGO_PROFILE_TEST_DEBUG ?= 0

ci: fmt clippy test test-python

fmt:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

test-python:
	python3 -m unittest discover -s tests -p 'ops*_test.py' -v

IMAGE ?= recipes-local:dev
container-build:
	docker build -t $(IMAGE) .
