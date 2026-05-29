.PHONY: build install link link-dev unlink dev fmt check clean

build:
	cargo build --release --bin opencli

install: build
	@./scripts/install-link.sh

# Link to the release binary — autosyncs after every `make build`.
link:
	@./scripts/install-link.sh

# Dev-mode wrapper that calls `cargo run` on each invocation — autosyncs as
# soon as you edit code (no manual build needed).
link-dev:
	@./scripts/install-link.sh --dev

unlink:
	@./scripts/uninstall-link.sh

# Run the app (TUI) in dev mode — rebuilds on each invocation.
dev:
	cargo run --bin opencli

fmt:
	cargo fmt --all

check:
	cargo check --workspace
	cargo clippy --workspace --all-targets -- -D warnings

clean:
	cargo clean
