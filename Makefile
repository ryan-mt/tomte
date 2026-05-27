.PHONY: build build-ui install link link-dev unlink dev fmt check clean

build:
	cargo build --release --bin opencli

build-ui:
	cd ui && npm install && npm run build

install: build build-ui
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

# Run the UI dev mode (Vite HMR) alongside the backend server.
dev:
	@echo "→ Run: cargo run -- ui --no-open  (in one terminal)"
	@echo "→ Run: cd ui && npm run dev       (in another terminal)"
	@echo "→ Open http://127.0.0.1:5173"

fmt:
	cargo fmt --all

check:
	cargo check --workspace
	cargo clippy --workspace --all-targets -- -D warnings

clean:
	cargo clean
	rm -rf ui/dist ui/node_modules
