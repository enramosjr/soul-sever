.PHONY: test clippy run

test:
	cargo test --workspace --all-targets

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

run:
	cargo run --quiet
