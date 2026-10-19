CARGO ?= cargo
PREFIX ?= /usr/local
DESTDIR ?=

.PHONY: build install clean test test-unit test-static check build-autocomplete-scripts install-autocomplete-scripts

build:
	$(CARGO) build --release
	cp target/release/menderctl ./menderctl

install: build
	install -d $(DESTDIR)$(PREFIX)/bin
	install -m 755 target/release/menderctl $(DESTDIR)$(PREFIX)/bin/menderctl

clean:
	$(CARGO) clean
	rm -f menderctl menderctl.linux.amd64 menderctl.darwin.arm64

test test-unit:
	$(CARGO) test --all-targets

test-static:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --all-targets -- -D warnings

check: test-static test-unit

build-autocomplete-scripts: build
	mkdir -p autocomplete
	./menderctl completion bash > autocomplete/autocomplete.sh
	./menderctl completion zsh > autocomplete/autocomplete.zsh

install-autocomplete-scripts: build-autocomplete-scripts
	install -d $(DESTDIR)$(PREFIX)/etc/bash_completion.d
	install -m 644 autocomplete/autocomplete.sh $(DESTDIR)$(PREFIX)/etc/bash_completion.d/menderctl
	install -d $(DESTDIR)$(PREFIX)/share/zsh/site-functions
	install -m 644 autocomplete/autocomplete.zsh $(DESTDIR)$(PREFIX)/share/zsh/site-functions/_menderctl

