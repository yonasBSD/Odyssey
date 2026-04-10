<div align="center">
  <img src="assets/logo.png" alt="Odyssey logo" width="180" height="180">

  # Odyssey

  <p><strong>Bundle-first AI agents in Rust.</strong></p>
  <p>Build agents, package them as portable bundles, and run them through one runtime.</p>

  [![Crates.io](https://img.shields.io/crates/v/odyssey-rs.svg)](https://crates.io/crates/odyssey-rs)
  [![License](https://img.shields.io/github/license/liquidos-ai/odyssey)](https://github.com/liquidos-ai/odyssey/blob/main/APACHE_LICENSE)
  [![CI](https://github.com/liquidos-ai/odyssey/actions/workflows/ci-chek.yml/badge.svg)](https://github.com/liquidos-ai/odyssey/actions/workflows/ci-chek.yml)
  [![Coverage](https://github.com/liquidos-ai/odyssey/actions/workflows/coverage.yml/badge.svg)](https://github.com/liquidos-ai/odyssey/actions/workflows/coverage.yml)
  [![Codecov](https://codecov.io/gh/liquidos-ai/Odyssey/graph/badge.svg)](https://codecov.io/gh/liquidos-ai/Odyssey)
  [![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/liquidos-ai/Odyssey)

  [Documentation](https://liquidos-ai.github.io/Odyssey/) | [Contributing](CONTRIBUTING.md)

  <br />
  <strong>Like this project?</strong> <a href="https://github.com/liquidos-ai/Odyssey">Star us on GitHub</a>
</div>

---

> **Status:** Odyssey is under active development and should still be treated as pre-production software.

Odyssey is an open-source, bundle-first agent runtime written in Rust on top of
[AutoAgents](https://github.com/liquidos-ai/AutoAgents). It lets you define an agent once, package
it as a portable bundle, and run it through the same runtime contract across local CLI workflows,
the TUI, HTTP server deployments, and embedded Rust applications.

Odyssey supports two practical authoring paths today:

- **Prompt agents** for fast local scaffolding and straightforward bundle workflows
- **Rust-authored custom agents** compiled to **WASM components** and hosted by the Odyssey runtime

For custom agents, Odyssey runs your Rust agent logic as a Wasmtime-hosted WebAssembly component
while keeping model credentials, tool execution, approvals, session state, and sandbox policy on
the host side. The WASM module owns behavior; Odyssey owns privileged effects.

## Why Odyssey

- **Portable bundles:** ship the agent spec, skills, resources, README, and runtime policy as one
  artifact.
- **Custom Rust WASM agents:** build agent logic with `odyssey-rs-agent-sdk` and run it through the
  same runtime as built-in execution paths.
- **Runtime-owned security boundary:** model access, tool brokering, approvals, and sandbox policy
  stay outside the agent component.
- **One engine, many surfaces:** CLI, TUI, HTTP, and embedded Rust all sit on top of the same
  runtime primitives.
- **Operational workflows included:** local install, inspect, export/import, and hub push/pull are
  built in.

## What Ships Today

- Prompt bundles with ReAct execution
- Rust-authored `kind: wasm` agents hosted through Wasmtime
- Stable Rust WASM agent SDK via `odyssey-rs-agent-sdk`
- Automatic `cargo component build` integration during bundle builds for local WASM agents
- Built-in tools: `Read`, `Write`, `Edit`, `LS`, `Glob`, `Grep`, `Bash`, and `Skill`
- Bundle manifests, validation, install, inspect, export, import, publish, and pull
- Session persistence, approvals, model resolution, and event streaming
- CLI, HTTP server, and Ratatui-based TUI
- Embeddable runtime via `odyssey-rs-runtime::OdysseyRuntime`

## At A Glance

| Surface | Purpose |
| --- | --- |
| `odyssey-rs run` | Run one prompt against a bundle |
| `odyssey-rs tui` | Local operator workflow with approvals, sessions, and bundle switching |
| `odyssey-rs serve` | Shared runtime over HTTP |
| `OdysseyRuntime` | Embed Odyssey directly into a Rust application |

## Quickstart

### Install the CLI

Bootstrap install:

```bash
curl -fsSL https://raw.githubusercontent.com/liquidos-ai/odyssey/main/install.sh | bash
```

Cargo install:

```bash
cargo install odyssey-rs
```

From a source checkout, use:

```bash
cargo run -p odyssey-rs --
```

### Create a starter bundle

```bash
odyssey-rs init ./hello-world
```

`init` currently creates a **prompt-based starter bundle** so you can get to a runnable project
immediately. It scaffolds:

- `odyssey.bundle.yaml`
- `agents/<bundle-id>/agent.yaml`
- `README.md`
- `skills/`
- `resources/`

### Build and install locally

```bash
odyssey-rs build ./hello-world
```

Build to a custom output directory instead:

```bash
odyssey-rs build ./hello-world --output ./dist
```

### Run the bundle

```bash
export OPENAI_API_KEY="your-key"
odyssey-rs run hello-world@latest --prompt "What can you do?"
```

### Inspect or browse installed bundles

```bash
odyssey-rs inspect hello-world@latest
odyssey-rs bundles
```

### Launch the TUI

```bash
export OPENAI_API_KEY="your-key"
odyssey-rs tui --bundle hello-world@latest
```

### Start a local runtime server

```bash
odyssey-rs serve --bind 127.0.0.1:8472
```

Target that runtime remotely:

```bash
odyssey-rs --remote http://127.0.0.1:8472 bundles
odyssey-rs --remote http://127.0.0.1:8472 inspect hello-world@latest
odyssey-rs --remote http://127.0.0.1:8472 run hello-world@latest --prompt "Summarize this bundle"
odyssey-rs --remote http://127.0.0.1:8472 sessions
```

## Custom Rust WASM Agents

Odyssey already supports custom Rust-authored agents compiled as WebAssembly components.

The current model is:

- your agent logic is compiled into `module.wasm`
- Odyssey instantiates that component in Wasmtime
- Odyssey provides host bindings for LLM calls, tool calls, and emitted events
- Odyssey still owns credentials, tool execution, session state, approvals, and sandbox policy

That gives you a portable custom-agent artifact without handing the component direct access to host
credentials or direct host tool handles.

### Recommended starting points

- [`bundles/hello-world`](bundles/hello-world): minimal prompt bundle
- [`bundles/odyssey-agent`](bundles/odyssey-agent): Rust-authored WASM workspace assistant
- [`bundles/code-act`](bundles/code-act): Rust-authored WASM CodeAct example

### Authoring layout

Typical WASM bundle layout:

```text
<project>/
  odyssey.bundle.yaml
  README.md
  agents/
    <agent-id>/
      agent.yaml
      Cargo.toml
      src/
        lib.rs
      module.wasm
  skills/
  resources/
```

### Minimal Rust agent

```rust
use autoagents_derive::{AgentHooks, agent};

#[agent(
    name = "my-agent",
    description = "Custom Odyssey agent",
    tools = [],
)]
#[derive(Default, Clone, AgentHooks)]
pub struct MyAgent;

fn app() -> odyssey_rs_agent_sdk::OdysseyAgentApp<MyAgent, odyssey_rs_agent_sdk::ReactExecutor> {
    odyssey_rs_agent_sdk::OdysseyAgentApp::react(MyAgent::default())
        .memory_window(20)
        .max_turns(12)
}

odyssey_rs_agent_sdk::export_odyssey_agent!("my-agent", app());
```

If you need agent-specific Rust tools, add them in the WASM crate with `.tool(...)`. Built-in
Odyssey tools declared in `agent.yaml` are injected automatically by the host runtime.

### Minimal bundle descriptors

`odyssey.bundle.yaml`:

```yaml
apiVersion: odyssey.ai/bundle.v1
kind: AgentBundle
metadata:
  name: my-agent
  version: 0.1.0
spec:
  abiVersion: v3
  agents:
    - id: my-agent
      spec: agents/my-agent/agent.yaml
      module: agents/my-agent/module.wasm
      default: true
```

`agents/my-agent/agent.yaml`:

```yaml
apiVersion: odyssey.ai/v1
kind: Agent
metadata:
  name: my-agent
  version: 0.1.0
spec:
  kind: wasm
  abiVersion: v3
  program:
    runner_class: wasm-component
    entrypoint: agents/my-agent/module.wasm
  execution:
    executor: react/v1
    memory: session-window/v1
```

### Build flow

Install the component tool:

```bash
cargo install cargo-component
```

Then build the bundle normally:

```bash
odyssey-rs build ./bundles/odyssey-agent
```

For local WASM agents, Odyssey detects `Cargo.toml` next to the agent spec, invokes
`cargo component build --release`, stages the produced `.wasm` artifact into the declared
`program.entrypoint`, validates that the entrypoint is a real WebAssembly component, and then
packages the bundle.

That means a normal bundle build is also the developer loop for Rust-authored custom agents.

## Sandboxing And Security Model

Odyssey treats the runtime as the trusted boundary, not the agent component.

### What the runtime owns

- model provider construction and credentials
- host tool execution
- approval gating
- session persistence
- sandbox preparation and policy enforcement
- CLI/TUI/HTTP event streaming

### What the agent owns

- prompts and behavior
- executor loop decisions inside prompt or WASM agent logic
- optional custom Rust tools compiled into the WASM component

### Sandbox modes

| Mode | Behavior |
| --- | --- |
| `read_only` | Confined runtime with read-only app workspace and explicit writable runtime state |
| `workspace_write` | Confined runtime with writable workspace/state areas |
| `danger_full_access` | Host execution without confined sandbox restrictions |

Important notes:

- Confined `read_only` and `workspace_write` execution is currently **Linux-only** and uses
  `bubblewrap`.
- On macOS and Windows, use `--dangerous-sandbox-mode` for local development or run Odyssey inside
  a Linux container if you need confined execution.
- Tool permissions are controlled through `tools.require`, `tools.allow`, `tools.ask`, and
  `tools.deny`.
- Filesystem and process policy are controlled through manifest sandbox settings such as
  `mounts.read`, `mounts.write`, `filesystem.exec`, `system_tools_mode`, and `system_tools`.
- `sandbox.env` is an allowlist-style passthrough for sandboxed commands, not a general process
  environment dump.

For the full policy reference, see:

- [Bundle Format](docs/content/reference/bundle-format.mdx)
- [Sandbox And Tools](docs/content/reference/sandbox-and-tools.mdx)
- [Runtime Architecture](docs/content/runtime/architecture.mdx)

## Bundle Lifecycle

Build and install locally:

```bash
odyssey-rs build ./bundles/hello-world
```

Export a portable archive:

```bash
odyssey-rs export local/hello-world@0.1.0 --output ./dist
```

Import a portable archive:

```bash
odyssey-rs import ./dist/hello-world-0.1.0.odyssey
```

Push and pull through a hub are also supported:

```bash
odyssey-rs push ./bundles/odyssey-agent --to acme/odyssey-agent@0.1.0
odyssey-rs pull acme/odyssey-agent@0.1.0
```

## Current Limits

- `odyssey-rs init` scaffolds a prompt agent today, not a WASM starter crate.
- Restricted sandboxes are Linux-only in the current release.
- Rust is the supported path for custom WASM agent authoring today.
- One selected agent is executed per run, even though a bundle may package many agents.
- WASM execution is runtime-owned and in-process; hardening around stronger execution limits and
  cancellation is still evolving.

## Repository Layout

- `crates/odyssey-rs`: CLI entrypoint and facade crate, including `run`, `serve`, and `tui`
- `crates/odyssey-rs-agent-sdk`: stable Rust SDK for Odyssey WASM agents
- `crates/odyssey-rs-agent-abi`: WIT and ABI types for Odyssey agent host bindings
- `crates/odyssey-rs-manifest`: bundle manifest and agent spec parsing/validation
- `crates/odyssey-rs-bundle`: build, install, inspect, export, import, publish, and pull
- `crates/odyssey-rs-runtime`: sessions, execution, prompt assembly, WASM hosting, tools, and
  sandbox integration
- `crates/odyssey-rs-tools`: built-in tools and adaptors
- `crates/odyssey-rs-sandbox`: sandbox runtime and providers
- `crates/odyssey-rs-server`: Axum-based HTTP API
- `crates/odyssey-rs-tui`: Ratatui-based terminal UI used by `odyssey-rs tui`
- `bundles/hello-world`: minimal prompt bundle
- `bundles/odyssey-agent`: WASM workspace assistant example
- `bundles/code-act`: WASM CodeAct example
- `examples/hello-world`: Rust embedding example

## Development

### Prerequisites

- Rust toolchain
- `cargo-component` if you are building Rust-authored WASM agents
- `rg`
- `tokei`
- `cargo-tarpaulin` if you want local coverage reports
- Docker Desktop or another recent Docker engine if you need Linux sandbox workflows on macOS
- `bubblewrap` (`bwrap`) on Linux for restricted local sandbox execution

### Common Commands

Format:

```bash
cargo fmt --all
```

Clippy:

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Workspace tests:

```bash
cargo test --all-features
```

Coverage:

```bash
cargo tarpaulin --engine llvm --skip-clean --workspace --all-features --timeout 120 --out Html
```

Example WASM component build:

```bash
cargo component build \
  --manifest-path bundles/odyssey-agent/agents/odyssey-agent/Cargo.toml \
  --release
```

## Documentation

- [Documentation site](https://liquidos-ai.github.io/Odyssey/)

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for development workflow,
quality expectations, and pull request guidance.

## Community

- **GitHub Issues:** bug reports and feature requests
- **Discussions:** community Q&A and design discussion
- **Discord:** [discord.gg/zfAF9MkEtK](https://discord.gg/zfAF9MkEtK)

## License

Odyssey is licensed under Apache 2.0. See [APACHE_LICENSE](APACHE_LICENSE).
