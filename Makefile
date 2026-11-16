# Per-user install (no root): binary in ~/.local/bin, user unit in
# ~/.config/systemd/user, extension in ~/.local/share/gnome-shell/extensions.
# For a system-wide install use the PKGBUILD in packaging/arch.

UUID     := arch-update-notifier@nodefive.github.io
EXT_SRC  := extension/$(UUID)
VERSION  := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)

XDG_DATA_HOME   ?= $(HOME)/.local/share
XDG_CONFIG_HOME ?= $(HOME)/.config

BINDIR   ?= $(HOME)/.local/bin
DATADIR  ?= $(XDG_DATA_HOME)
UNITDIR  ?= $(XDG_CONFIG_HOME)/systemd/user
DBUSDIR  ?= $(DATADIR)/dbus-1/services
EXTDIR   ?= $(DATADIR)/gnome-shell/extensions/$(UUID)
SNIPDIR  ?= $(DATADIR)/arch-update-notifier

UNIT     := arch-update-notifier.service
DBUSSVC  := io.github.nodefive.ArchUpdateNotifier.service
TARBALL  := packaging/arch/arch-update-notifier-$(VERSION).tar.gz

.PHONY: all build test schemas lint install uninstall dist zip clean

all: build schemas

build:
	cargo build --release --locked

test:
	cargo test --locked

schemas:
	glib-compile-schemas --strict $(EXT_SRC)/schemas

# Optional tools: shellcheck, node (syntax only; gi:// imports are not resolved).
lint:
	bash -n shell/arch-update-notifier.bash
	@if command -v shellcheck >/dev/null; then shellcheck -s bash shell/arch-update-notifier.bash; fi
	@if command -v node >/dev/null; then \
		for f in $(EXT_SRC)/*.js $(EXT_SRC)/lib/*.js; do \
			node --check --input-type=module < "$$f" || exit 1; \
		done; echo "js syntax ok"; fi
	glib-compile-schemas --strict --dry-run $(EXT_SRC)/schemas

install: build schemas
	install -Dm755 target/release/arch-update-notifier "$(BINDIR)/arch-update-notifier"
	install -d "$(UNITDIR)" "$(DBUSDIR)"
	sed 's|@BINDIR@|$(BINDIR)|' data/$(UNIT).in > "$(UNITDIR)/$(UNIT)"
	sed 's|@BINDIR@|$(BINDIR)|' data/$(DBUSSVC).in > "$(DBUSDIR)/$(DBUSSVC)"
	install -Dm644 shell/arch-update-notifier.bash "$(SNIPDIR)/arch-update-notifier.bash"
	install -d "$(EXTDIR)"
	cp -r $(EXT_SRC)/. "$(EXTDIR)/"
	@echo
	@echo "Installed. Next steps:"
	@echo "  systemctl --user daemon-reload"
	@echo "  systemctl --user enable --now $(UNIT)"
	@echo "  gnome-extensions enable $(UUID)   # after logging out and back in"
	@echo "  echo '[ -r $(SNIPDIR)/arch-update-notifier.bash ] && . $(SNIPDIR)/arch-update-notifier.bash' >> ~/.bashrc"

uninstall:
	@test -n "$(UUID)" && test -n "$(DATADIR)"
	-systemctl --user disable --now $(UNIT) 2>/dev/null
	rm -f "$(BINDIR)/arch-update-notifier" "$(UNITDIR)/$(UNIT)" "$(DBUSDIR)/$(DBUSSVC)"
	rm -f "$(SNIPDIR)/arch-update-notifier.bash"
	-rmdir "$(SNIPDIR)" 2>/dev/null
	rm -rf -- "$(EXTDIR)"
	-systemctl --user daemon-reload
	@echo "Cache left in $${XDG_CACHE_HOME:-$$HOME/.cache}/arch-update-notifier; settings in dconf:"
	@echo "  dconf reset -f /org/gnome/shell/extensions/arch-update-notifier/"

# Source tarball for packaging/arch/PKGBUILD.
dist: $(TARBALL)

$(TARBALL): Cargo.toml Cargo.lock Makefile README.md $(shell find src tests data shell extension -type f ! -name gschemas.compiled)
	tar --exclude=gschemas.compiled --transform 's,^,arch-update-notifier-$(VERSION)/,' \
		-czf $@ Cargo.toml Cargo.lock Makefile README.md src tests data shell extension
	@echo "Wrote $@"

# extensions.gnome.org style zip (schemas are compiled on install since GNOME 44).
zip:
	mkdir -p dist
	gnome-extensions pack $(EXT_SRC) --force --out-dir=dist \
		--extra-source=lib --extra-source=icons

clean:
	cargo clean
	rm -f $(EXT_SRC)/schemas/gschemas.compiled
	rm -rf dist
