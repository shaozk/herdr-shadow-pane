.PHONY: build install uninstall

build:
	./scripts/build.sh

install:
	herdr plugin link .

uninstall:
	herdr plugin unlink shaozk.sync-panes
