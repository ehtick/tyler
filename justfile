#!/usr/bin/env just --justfile

_default:
    just --list

# Fast compile-time validation across the workspace.
check:
    cargo check --workspace --all-targets --all-features --exclude proj-sys

# Build all workspace targets with all features enabled.
build *args:
    cargo build --workspace --all-targets --all-features --exclude proj-sys {{args}}

# Run clippy with strict lint settings across the workspace.
lint:
    cargo clippy --workspace --exclude proj-sys --no-deps -- -Dclippy::all -Dclippy::pedantic

# Format the workspace with rustfmt.
fmt:
    cargo fmt --package tyler --package cityjson-convert

# Run the workspace tests with all features enabled.
test:
    cargo test --workspace --all-targets --all-features --exclude proj-sys

# Clean the workspace by removing all build artifacts.
clean:
    cargo clean

# Run the full local validation sequence.
ci:
    just fmt
    just lint
    just check
    just build
    just test
