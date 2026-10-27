# dev-box

A [DBX](https://dbxio.com) plugin that provisions local development environments — JDK, Maven, MySQL,
Redis, Nginx, RocketMQ, Nacos, xxl-job-admin — with multiple versions coexisting and switching that
takes milliseconds.

[中文说明](./README.zh-CN.md)

> **Status: early.** The download path is real and verified end to end. Most components are not
> yet installable because their catalog entries still carry placeholder checksums — by design.
> See [Current state](#current-state).

---

## Why another environment manager

Three ideas drive the design:

1. **Artifacts, instances and activation are different things.** A version is a read-only directory
   that never changes. An instance is data plus config that you can create and destroy freely.
   Activation is a pointer. Mixing them is why so many tools get "switch version" wrong.
2. **The catalog is data, not code.** Adding a component should mean writing YAML, not touching the
   engine. The engine knows nothing about MySQL.
3. **Installing software is a supply-chain action.** Everything is pinned by checksum, archives are
   treated as hostile, and nothing runs through a shell.

Design document: [`docs/plugin-architecture.md`](./docs/plugin-architecture.md) (Chinese).

---

## Current state

| Area | State |
|---|---|
| Download → verify → extract → atomic commit | **Working**, verified with real bytes |
| Resume after interruption | **Working** (206 append / 200 overwrite / 416 discard) |
| Checksum enforcement | sha256 and sha512, placeholders rejected at parse time |
| Mirror fallback | User-configured first, then original; 404 does *not* fall back |
| Catalog (YAML) | Parsed; changing a recipe needs no code change |
| Verified recipes | **Maven 3.9.9 / 3.8.8**, **Node.js 22.11.0** — everything else is still a placeholder |
| Version switching | **Working**: `current/<component>` link flips between installed versions in milliseconds |
| PATH integration | **Working**: `$DATA/bin` holds shims that forward through `current/`, so switching versions needs no PATH change |
| Dependency resolution | Logic + tests done, not yet wired into the install flow |
| Container (Docker) backend | Planning only — no `docker` calls yet |
| Persistence (`detached` / `autostart`) | Not implemented (needs daemon mode) |
| UI | Renders from mock data; real catalog wiring is next |

Installing anything other than Maven currently fails with an honest error such as:

```
jdk 21.0.5 (windows-x64) 的校验值仍是占位符，说明这份配方还没被真正验过
```

That is intentional: an unverified recipe should not install successfully.

---

## Build and test

Requires Rust (2021 edition) and Node 18+ only if you work on the UI.

```bash
# unit tests: 76 total
cargo test --manifest-path backend/core/Cargo.toml
cargo test --manifest-path backend/engine/Cargo.toml

# sidecar (the DBX plugin backend)
cargo build --manifest-path backend/sidecar/Cargo.toml
```

### Install something for real

The CLI is the quickest way to exercise the whole pipeline:

```bash
cargo build --manifest-path backend/engine/Cargo.toml --bin devbox-install

./backend/engine/target/debug/devbox-install \
  --catalog catalog/jdk.yaml \
  --component maven --version 3.9.9 \
  --data /tmp/devbox-data
```

Then generate the command-line entry points:

```bash
./backend/engine/target/debug/devbox-shim --data /tmp/devbox-data --catalog catalog/jdk.yaml
```

It prints the one line you need to add to your shell config. We never edit it for you.

Expected output:

```
目录 rev 2026.10.27-m0 · schema 1
→ maven 3.9.9 (windows-x64)  https://...
✓ 安装完成： 90 个文件，8.78 MB
  产物目录: /tmp/devbox-data/store/maven/3.9.9/windows-x64
```

### Run the sidecar over JSONL

```bash
echo '{"id":1,"method":"devbox/install","params":{"component":"maven","version":"3.9.9"}}' \
  | DEVBOX_CATALOG=catalog/jdk.yaml DBX_PLUGIN_DATA_DIR=/tmp/devbox \
    ./backend/sidecar/target/debug/devbox-sidecar
```

Progress arrives as `evt/job.progress` events; `devbox/job.get` lets a reconnected UI resume
displaying a job it did not start.

---

## Layout

```
manifest.json            DBX plugin manifest (workbench contribution)
dbx-plugin.toml          build config
catalog/                 component recipes (YAML) — shipped inside the package
ui/                      sandboxed console: vanilla ESM, no build step
backend/core/            pure logic: state machine, paths, platforms, volumes, deps
backend/engine/          real IO: download, verify, extract, atomic commit
backend/sidecar/         DBX sidecar: JSONL protocol + engine wiring
docs/                    architecture document
```

Why `core` is separate: it has **zero dependencies**, so `cargo test` runs anywhere and the rules
that matter (state transitions, path safety, version ranges) are testable without a network or a
sandbox.

---

## Security notes

- HTTPS only. Plain `http://` URLs are rejected before any request is made.
- Every artifact is pinned by sha256/sha512; mirrors may change the host, never the bytes.
- Archives are treated as hostile: absolute paths, `..`, symlinks, hard links, Windows device names
  and alternate data streams are rejected per entry; extraction is bounded by entry count and total
  size.
- Commands are executed as `argv` arrays. No shell, no string interpolation.
- Container ports bind to `127.0.0.1`. Mounting `/proc`, `/sys`, `/dev`, `/etc` or the Docker socket
  is refused.
- The tool never runs `sudo` and never edits your shell profile, registry or Docker daemon config.

---

## A note on mirrors

Measured from a network in mainland China while building this, for the Maven 3.9.9 artifact:

| Source | Result |
|---|---|
| `repo.huaweicloud.com` | ~2.4 MB/s, works |
| `mirrors.huaweicloud.com` | ~1.4 MB/s, works |
| `archive.apache.org` | ~20 KB/s, effectively unusable |
| Aliyun / Tencent / USTC / Apache CDN | 404 for this path |

Mirror lists must be measured, not assumed. This is also why a 404 does not trigger fallback: four
404s in a row would waste eight seconds and hide the real error.

---

## License

TBD — decide before publishing to the store.
