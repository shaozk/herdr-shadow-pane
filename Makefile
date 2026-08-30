.DEFAULT_GOAL := help
.PHONY: build install uninstall help

help: ## List available targets
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / {printf "  %-12s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Build release binary into bin/
	./scripts/build.sh

install: ## Link this repo as a herdr plugin
	herdr plugin link .

uninstall: ## Unlink the herdr plugin
	herdr plugin unlink shaozk.sync-panes
