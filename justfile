default: test

build:
    cargo build --workspace

test:
    cargo test --workspace

lint:
    cargo fmt --all -- --check
    cargo clippy --all-targets -- -D warnings

fmt:
    cargo fmt --all

# Run the dev server against .dev-data (registry, blobs, outbox, secret).
dev:
    cargo run -p finitesitesd -- serve --data .dev-data

# Add an operator publish grant on the dev server's data dir.
allow npub note="dev":
    cargo run -p finitesitesd -- allow --data .dev-data {{npub}} --note "{{note}}"

allowed:
    cargo run -p finitesitesd -- allowed --data .dev-data

# Wipe local dev state.
clean-dev:
    rm -rf .dev-data
