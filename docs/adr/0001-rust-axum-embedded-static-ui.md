# 0001 One Rust binary with an embedded static UI

**Context.** An earlier prototype used Go plus a Vue/Vite app. The owner wants the greprum stack and style, no Node, and a typed REST API.
**Decision.** Rust (axum) serves a typed API and embeds hand-written HTML, CSS and ES modules. No frontend build step.
**Consequences.** One toolchain and no npm supply chain. All tests are Rust. UI logic stays thin; charts are written in the repo.
