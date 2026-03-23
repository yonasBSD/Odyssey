# Cargo Publish Guide for Odyssey

**Follow the below instructions in sequence**

1. Create a release branch from `main`:

```shell
git checkout main
git pull origin main
git checkout -b feature/vx.x.x
```

```shell
git add .
git commit -m "[MAINT]: bump version to x.x.x"
git push origin feature/vx.x.x
```

10. Publish to crates.io from `main` and MAINTAIN the order below.
    Run `cargo publish --dry-run` before each real publish.

```shell
cd crates/odyssey-rs-protocol
cargo publish --dry-run
cargo publish
```

```shell
cd ../odyssey-rs-manifest
cargo publish --dry-run
cargo publish
```

```shell
cd ../odyssey-rs-sandbox
cargo publish --dry-run
cargo publish
```

```shell
cd ../odyssey-rs-bundle
cargo publish --dry-run
cargo publish
```

```shell
cd ../odyssey-rs-tools
cargo publish --dry-run
cargo publish
```

```shell
cd ../odyssey-rs-runtime
cargo publish --dry-run
cargo publish
```

```shell
cd ../odyssey-rs-server
cargo publish --dry-run
cargo publish
```

```shell
cd ../odyssey-rs-tui
cargo publish --dry-run
cargo publish
```

```shell
cd ../odyssey-rs
cargo publish --dry-run
cargo publish
```

11. Wait for crates.io to index each crate before publishing the next dependent crate.
    If a dependent publish fails because the previous crate version is not visible yet, wait briefly and retry.

12. Create the release tag on the merged `main` commit:

```shell
cd ../..
git push -u origin feature/v.x.x
git checkout main
git pull
git tag -a vx.x.x -m "Release vx.x.x

Features:
-

Improvements:
-
"
```

```shell
git push origin vx.x.x
```
