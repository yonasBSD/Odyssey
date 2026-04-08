# Odyssey Hello World

This is an Hello World Agent Bundle 

## Build

```bash
cargo run -p odyssey-rs -- build bundles/hello-world
```

Or write the built bundle to a custom directory:

```bash
cargo run -p odyssey-rs -- build bundles/hello-world --output ./dist
```

## Run

```bash
cargo run -p odyssey-rs -- run hello-world@latest --prompt "Hey, What is your name?"
```

The hello-world bundle now uses the local `llama_cpp` provider with the
`unsloth/Qwen3.5-0.8B-GGUF` Hugging Face repo and the
`Qwen3.5-0.8B-UD-Q4_K_XL.gguf` model file. The first run may download the model
and initialize llama.cpp. If the Hugging Face repo requires authentication,
export `HUGGINGFACE_TOKEN`, `HF_TOKEN`, or `HUGGINGFACE_HUB_TOKEN` before
running the bundle.

Host filesystem mounts are configured under `sandbox.permissions.filesystem.mounts`.
The default cowork bundle leaves both `read` and `write` empty, so host volume access is denied by default.
