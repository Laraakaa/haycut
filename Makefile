.PHONY: check build run test install clean fmt clippy help

help:
	@echo "Available targets:"
	@echo "  make check    - Fast compile/type check"
	@echo "  make build    - Build debug binary"
	@echo "  make run      - Run the CLI with cargo run"
	@echo "  make test     - Run tests"
	@echo "  make install  - Install haycut into ~/.cargo/bin"
	@echo "  make clean    - Remove Cargo build artifacts"
	@echo "  make fmt      - Format Rust code"
	@echo "  make clippy   - Run Rust lints"

check:
	cargo check

build:
	cargo build

run:
	cargo run -- $(ARGS)

test:
	cargo test

install:
	cargo install --path . --force

clean:
	cargo clean

fmt:
	cargo fmt

clippy:
	cargo clippy --all-targets --all-features
