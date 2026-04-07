# Build both Rust and Elm.
build: build-elm build-rust

# Build the Elm frontend.
build-elm:
    cd frontend && elm make src/Main.elm --output public/elm.js

# Build all Rust workspace crates.
build-rust:
    cargo build --workspace

# Run all tests (Elm compile check + Rust + Emacs test suites).
test: build-elm test-rust test-emacs

# Run the Rust test suite.
test-rust:
    cargo test --workspace

# Run the Emacs ERT test suite.
test-emacs:
    emacs --batch -L emacs -l emacs/hyuqueue-tests.el -f ert-run-tests-batch-and-exit

# Build Elm then run via cargo, forwarding all arguments.
run *args: build-elm
    cargo run {{args}}
