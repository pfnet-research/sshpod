CARGO ?= cargo
DOCKER ?= docker
INSTALL_ROOT ?= $(HOME)/.local
BUNDLES_DIR ?= $(CURDIR)/bundles
ARCHES ?= amd64 arm64
OPENSSH_VERSION ?= 9.7p1
BUNDLE_FILES := $(foreach arch,$(ARCHES),$(BUNDLES_DIR)/sshd_$(arch).xz)

.PHONY: all build install lint format check clean bundles test

all: build

build:
	$(CARGO) build --release

install: build
	./target/release/sshpod configure
	$(CARGO) install --path . --locked --root $(INSTALL_ROOT)

lint:
	$(CARGO) clippy --all-targets -- --deny=warnings
	$(CARGO) fmt -- --check

format:
	$(CARGO) fmt

check:
	$(CARGO) check

test:
	$(CARGO) test --all-features

clean:
	$(CARGO) clean

bundles: $(BUNDLE_FILES)

$(BUNDLES_DIR)/sshd_%.xz: Dockerfile.bundle
	@mkdir -p $(dir $@)
	@set -euo pipefail; \
	ARCH="$*"; \
	PLATFORM="linux/$$ARCH"; \
	BUNDLE_FILE="$(notdir $@)"; \
	echo "Building bundle $$BUNDLE_FILE for $$PLATFORM"; \
	DOCKER_BUILDKIT=1 $(DOCKER) build --platform $$PLATFORM \
		--build-arg OPENSSH_VERSION=$(OPENSSH_VERSION) \
		--build-arg BINARY_FILENAME=$$BUNDLE_FILE \
		-t sshpod-bundle-$$ARCH \
		-f Dockerfile.bundle .; \
	CID="$$( $(DOCKER) create sshpod-bundle-$$ARCH )"; \
	$(DOCKER) cp $$CID:/out/$$BUNDLE_FILE "$@"; \
	$(DOCKER) rm $$CID >/dev/null
