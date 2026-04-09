# Odyssey Agent Bundle

This is a Rust-authored WASM general assistant bundle for Odyssey.

`agents/odyssey-agent/src/lib.rs` uses the stable `odyssey-rs-agent-sdk` surface:

- `OdysseyAgentApp::react(...)` wires the host LLM provider, memory preload, and component runner
- tools declared in `agents/odyssey-agent/agent.yaml` are injected automatically from the host
- Odyssey agent crates only add Rust-defined custom tools when they need agent-specific behavior

## Build The WASM Module

```bash
cargo component build \
  --manifest-path bundles/odyssey-agent/agents/odyssey-agent/Cargo.toml \
  --release
```

`cargo-component` writes the component artifact into Cargo's target directory. When you run
`odyssey-rs build bundles/odyssey-agent`, Odyssey now invokes `cargo component build` for this
agent automatically and stages the resulting component into
`agents/odyssey-agent/module.wasm` before validating and packaging the bundle.

## Build

```bash
cargo run -p odyssey-rs -- build bundles/odyssey-agent
```

Or write the built bundle to a custom directory:

```bash
cargo run -p odyssey-rs -- build bundles/odyssey-agent --output ./dist
```

## Run

```bash
export OPENAI_API_KEY="your-key"
cargo run -p odyssey-rs -- run odyssey-agent@latest --prompt "Heyy, There!"
```

Host filesystem mounts are configured under `sandbox.permissions.filesystem.mounts`.
The default cowork bundle leaves both `read` and `write` empty, so host volume access is denied by default.
