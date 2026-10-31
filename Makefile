.DEFAULT_GOAL := help
BINARY := grepui

# Single source of truth for the version (no external tooling required).
VERSION_CURRENT := $(shell cat VERSION 2>/dev/null || echo 0.0.0)
GREPTIME_VERSION ?= v1.2.1
GREPTIME_NAME := grepui-db
IMAGE ?= ghcr.io/teochenglim/grepui

.PHONY: help
help: ## Show this menu
	@echo "grepui $(VERSION_CURRENT) - available targets:"
	@grep -E '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'
	@echo ""
	@echo "Release cycle:"
	@echo "  make release VERSION=x.y.z   # bump VERSION, push HEAD, tag, push tag -> CI"

## --- develop -----------------------------------------------------------

.PHONY: build
build: ## Build the release binary into ./bin (the UI in web/static/ is embedded; no JS toolchain)
	GREPUI_VERSION=v$(VERSION_CURRENT) cargo build --release --locked
	@mkdir -p bin && cp target/release/$(BINARY) bin/$(BINARY)

.PHONY: run
run: ## Run against a local GreptimeDB (make db-up && make seed first); UI on http://127.0.0.1:8080
	cargo run -- serve

.PHONY: playground
playground: ## Playground against a local GreptimeDB: seeds the `demo` database, trickles live demo traffic, serves :8080
	cargo run -- playground --demo-data --wait 30 --greptime-mysql mysql://root@127.0.0.1:4002/demo

.PHONY: test
test: ## Run the Rust test suite (unit + AC tests, no DB needed)
	cargo test --locked

.PHONY: test-db
test-db: ## Live tests against GreptimeDB (run `make db-up` and `make seed` first)
	GREPUI_TEST_DB=mysql://root@127.0.0.1:4002/public cargo test --locked --no-fail-fast -- --ignored --test-threads=1

.PHONY: seed
seed: ## Send demo traces + logs to the local GreptimeDB over OTLP (ORG=tenant_a for another database)
	python3 scripts/seed.py $(if $(ORG),--db $(ORG),)

.PHONY: ac-map
ac-map: ## Traceability gate: every requirement -> AC -> >=2 tests
	./scripts/ac-map.sh

.PHONY: fmt
fmt: ## Format Rust source
	cargo fmt --all

.PHONY: fmt-check
fmt-check: ## Check formatting
	cargo fmt --all -- --check

.PHONY: clippy
clippy: ## Lint (warnings are errors)
	cargo clippy --all-targets --locked -- -D warnings

.PHONY: no-banned-names
no-banned-names: ## Fail if a name we must never mention appears anywhere in the tree
	cargo test --locked --test ac_delivery ac_0_1_12_no_banned_name

.PHONY: check
check: fmt-check clippy ac-map test version-check ## Everything CI runs (includes the banned-name test)

.PHONY: clean
clean: ## Remove local build artifacts (cargo target/, bin/)
	cargo clean
	rm -rf bin

## --- container -------------------------------------------------------------

.PHONY: docker-build
docker-build: ## Build the container image (tag :$(VERSION_CURRENT))
	docker build --build-arg VERSION=v$(VERSION_CURRENT) -t $(IMAGE):$(VERSION_CURRENT) -t $(IMAGE):latest .

.PHONY: demo
demo: ## GreptimeDB + grepui (playground mode: seeds demo data itself) from docker compose (UI http://127.0.0.1:8080)
	docker compose up -d --build
	@echo "grepui: http://127.0.0.1:8080  (the first start seeds demo data before it listens)"

.PHONY: docker-push
docker-push: ## Manual push to GHCR (CI does this on tags). Needs `docker login ghcr.io` first.
	docker push $(IMAGE):$(VERSION_CURRENT)
	docker push $(IMAGE):latest

## --- local GreptimeDB ----------------------------------------------------

.PHONY: db-up
db-up: ## Start the pinned GreptimeDB in docker (HTTP 4000, MySQL 4002)
	-@docker rm -f $(GREPTIME_NAME) >/dev/null 2>&1
	docker run -d --name $(GREPTIME_NAME) -p 127.0.0.1:4000-4003:4000-4003 greptime/greptimedb:$(GREPTIME_VERSION) \
	  standalone start --http-addr 0.0.0.0:4000 --rpc-bind-addr 0.0.0.0:4001 \
	  --mysql-addr 0.0.0.0:4002 --postgres-addr 0.0.0.0:4003

.PHONY: db-down
db-down: ## Stop and remove the local GreptimeDB
	-docker rm -f $(GREPTIME_NAME)

## --- supply-chain hardening ----------------------------------------------

.PHONY: github-action-bump
github-action-bump: ## Pin .github/workflows/*.yml actions to the newest release of the same major, as full commit SHAs
	python3 scripts/pin-actions.py
	@echo "Actions bumped and verified. Review the diff, then run 'make check' before committing."

.PHONY: audit
audit: ## cargo-audit + cargo-deny (install separately)
	cargo audit
	cargo deny check

## --- release --------------------------------------------------------------

.PHONY: version
version: ## Print the version currently in VERSION
	@echo $(VERSION_CURRENT)

.PHONY: version-check
version-check: ## VERSION must equal the Cargo.toml package version
	@test "$$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')" = "$(VERSION_CURRENT)" \
	  || { echo "VERSION ($(VERSION_CURRENT)) != Cargo.toml version"; exit 1; }

.PHONY: bump
bump: ## Rewrite VERSION and Cargo.toml with the new version (VERSION=x.y.z required)
	@if [ -z "$(VERSION)" ]; then echo "Usage: make bump VERSION=x.y.z"; exit 1; fi
	@echo "$(VERSION)" > VERSION
	@perl -0pi -e 's/^version = ".*?"/version = "$(VERSION)"/m' Cargo.toml
	@cargo update -w >/dev/null 2>&1 || true
	@echo "VERSION -> $(VERSION)"

.PHONY: release
release: ## Bump VERSION, push HEAD, tag, push the tag - triggers GitHub Actions (VERSION=x.y.z required)
	@if [ -z "$(VERSION)" ]; then echo "Usage: make release VERSION=x.y.z"; exit 1; fi
	$(MAKE) bump VERSION=$(VERSION)
	git add VERSION Cargo.toml Cargo.lock
	git commit --amend --no-edit
	git push origin HEAD
	git tag v$(VERSION)
	git push origin v$(VERSION)
	@echo "Released v$(VERSION) - GitHub Actions will build and publish."
