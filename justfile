e2e_addr := env("HPTX_E2E_ADDR", "tcp://localhost:4848")

# List recipes
default:
    @just --list

# Fast test suite
test:
    cargo test --workspace -q

# Check formatting and run clippy
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

# Format all code
fmt:
    cargo fmt --all

# Emulator end-to-end tests (start the emulator first with `just emulator-up`)
e2e:
    HPTX_E2E_ADDR={{e2e_addr}} cargo test -p hptx-core --test e2e -- --nocapture

# e2e suite against the in-process saturnus emulator (HP 48SX ROM J path)
e2e-saturnus rom="../saturnus/roms/sxrom-j":
    HPTX_E2E_ADDR=saturnus://$(cd "$(dirname {{rom}})" && pwd)/$(basename {{rom}}) cargo test -p hptx-core --features saturnus --test e2e -- --nocapture

# e.g. `just record-trace dir > crates/kermit-proto/traces/48sx-dir.trace`
# Record a Kermit trace from the running emulator to stdout
record-trace +args:
    @cargo run -q -p kermit-proto --example record -- localhost:4848 {{args}}

# Build and start the emulator; model is 49g, 48gx or 48sx
emulator-up model="48sx":
    docker build -t hp49g-emu emulator
    docker run --rm -d -p 4848:4848 -e MODEL={{model}} --name calc hp49g-emu
    for i in $(seq 60); do docker logs calc 2>&1 | grep -q bridged && exit 0; sleep 1; done; \
        echo "emulator not bridged after 60 s" >&2; exit 1

# Stop and remove the emulator
emulator-down:
    -docker rm -f calc
