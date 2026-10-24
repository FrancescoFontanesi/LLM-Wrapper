# Build, install and package llm-wrapper.
#
#   make build                          release build into rust/dist (needs the GTK 4 / WebKitGTK 6.0 dev packages)
#   make install                        install under /usr/local (PREFIX, DESTDIR and BINDIR work as usual)
#   make install PREFIX=$HOME/.local    for one user, no root
#   make uninstall PREFIX=...           remove what `install` put there (not your data: see README)
#   make dist                           a tarball of the installed layout in the current directory
#   make test                           unit tests + lints (the end-to-end suite is rust/scripts/e2e.sh)
#
# The client (`llm-wrapper`) starts the daemon (`llm-wrapperd`) from the directory it lives in and from
# nowhere else, so the two are always installed together.

NAME     := llm-wrapper
PREFIX   ?= /usr/local
DESTDIR  ?=
BINDIR   ?= $(PREFIX)/bin
DATADIR  ?= $(PREFIX)/share
DIST     := rust/dist
VERSION  := $(shell sed -n 's/^version = "\(.*\)"/\1/p' rust/Cargo.toml | head -1)
ARCH     := $(shell uname -m)

.PHONY: build install uninstall dist test clean

build:
	rust/scripts/build-release.sh $(CURDIR)/$(DIST)

install:
	@test -x $(DIST)/llm-wrapper -a -x $(DIST)/llm-wrapperd || { echo "nothing built yet: run 'make build' first" >&2; exit 1; }
	install -Dm755 $(DIST)/llm-wrapper  $(DESTDIR)$(BINDIR)/llm-wrapper
	install -Dm755 $(DIST)/llm-wrapperd $(DESTDIR)$(BINDIR)/llm-wrapperd
	install -Dm644 packaging/llm-wrapper.desktop $(DESTDIR)$(DATADIR)/applications/llm-wrapper.desktop
	install -Dm644 README.md $(DESTDIR)$(DATADIR)/doc/$(NAME)/README.md
	install -Dm644 LICENSE   $(DESTDIR)$(DATADIR)/doc/$(NAME)/LICENSE

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/llm-wrapper $(DESTDIR)$(BINDIR)/llm-wrapperd
	rm -f $(DESTDIR)$(DATADIR)/applications/llm-wrapper.desktop
	rm -rf $(DESTDIR)$(DATADIR)/doc/$(NAME)

dist: build
	rm -rf $(DIST)/stage && mkdir -p $(DIST)/stage
	$(MAKE) install DESTDIR=$(CURDIR)/$(DIST)/stage PREFIX=/usr
	tar -C $(DIST)/stage --owner=0 --group=0 -czf $(NAME)-$(VERSION)-$(ARCH)-linux.tar.gz usr
	rm -rf $(DIST)/stage
	@echo "built $(NAME)-$(VERSION)-$(ARCH)-linux.tar.gz (extract at / or under any prefix; it needs gtk4, webkitgtk-6.0, bubblewrap, xdg-dbus-proxy)"

test:
	cd rust && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --bins --lib

clean:
	rm -rf $(DIST) rust/target
