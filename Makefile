PREFIX ?= $(HOME)/.local
BINDIR := $(PREFIX)/bin
LIBDIR := $(PREFIX)/lib/nns-vesl
WRAPPER := $(BINDIR)/nns
ALT_WRAPPER := $(BINDIR)/nns-vesl
BIN := $(LIBDIR)/nns-vesl
KERNEL := $(LIBDIR)/out.jam
SHELL_RC ?= $(HOME)/.zshrc
PATH_LINE := export PATH="$$HOME/.local/bin:$$PATH"

.PHONY: install uninstall

install:
	bash scripts/setup-hoon-tree.sh
	hoonc --new hoon/app/app.hoon hoon/
	cargo +nightly build --release
	install -d "$(DESTDIR)$(BINDIR)" "$(DESTDIR)$(LIBDIR)"
	install -m 755 "target/release/nns-vesl" "$(DESTDIR)$(BIN)"
	install -m 644 "out.jam" "$(DESTDIR)$(KERNEL)"
	printf '#!/usr/bin/env sh\nexport NNS_KERNEL_JAM=%s/out.jam\nexec %s/nns-vesl "$$@"\n' \
	  "$(LIBDIR)" "$(LIBDIR)" > "$(DESTDIR)$(WRAPPER)"
	chmod 755 "$(DESTDIR)$(WRAPPER)"
	ln -sf "nns" "$(DESTDIR)$(ALT_WRAPPER)"
	@touch "$(SHELL_RC)"
	@rg -qxF '$(PATH_LINE)' "$(SHELL_RC)" || printf '\n%s\n' '$(PATH_LINE)' >> "$(SHELL_RC)"
	@hash -r 2>/dev/null || true
	@printf '\nInstalled nns-vesl CLI:\n'
	@printf '  %s\n' "$(DESTDIR)$(WRAPPER)"
	@printf '  %s (alias)\n' "$(DESTDIR)$(ALT_WRAPPER)"
	@printf '\nUpdated %s with:\n' "$(SHELL_RC)"
	@printf '  %s\n' '$(PATH_LINE)'
	@printf 'Open a new shell if `nns` still resolves to another tool.\n'

uninstall:
	rm -f "$(DESTDIR)$(WRAPPER)"
	rm -f "$(DESTDIR)$(ALT_WRAPPER)"
	rm -f "$(DESTDIR)$(BIN)" "$(DESTDIR)$(KERNEL)"
	rmdir "$(DESTDIR)$(LIBDIR)" 2>/dev/null || true
