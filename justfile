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

# CLI end-to-end script against a running emulator, e.g. `just e2e-cli tcp://localhost:4852` (the 49G)
e2e-cli addr=e2e_addr:
    HPTX_E2E_ADDR={{addr}} scripts/e2e-cli.sh

# e.g. `just record-trace dir > crates/kermit-proto/traces/48sx-dir.trace`
# Record a Kermit trace from the running emulator to stdout
record-trace +args:
    @cargo run -q -p kermit-proto --example record -- localhost:4848 {{args}}

# Build and start the emulator; model is 49g, 48gx or 48sx; 49G: `just emulator-up 49g 4852 calc49`
emulator-up model="48sx" port="4848" name="calc":
    @# Fail fast if anything already listens on the port (another emulator would win on 127.0.0.1).
    @if command -v lsof >/dev/null && lsof -nP -iTCP:{{port}} -sTCP:LISTEN >/dev/null; then \
        echo "port {{port}} is already in use; pick another or stop it:" >&2; \
        lsof -nP -iTCP:{{port}} -sTCP:LISTEN >&2; exit 1; fi
    docker build -t hp49g-emu emulator
    docker run --rm -d -p {{port}}:4848 -e MODEL={{model}} --name {{name}} hp49g-emu
    for i in $(seq 60); do docker logs {{name}} 2>&1 | grep -q bridged && exit 0; sleep 1; done; \
        echo "emulator not bridged after 60 s" >&2; exit 1

# Stop and remove the emulators
emulator-down:
    -docker rm -f calc calc49
