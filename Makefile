BINARY     := hypr-overlay-wl
INSTALL_BIN := $(HOME)/.local/bin/$(BINARY)
SERVICE    := assets/hypr-overlay.service
INSTALL_SVC := $(HOME)/.config/systemd/user/hypr-overlay.service

.PHONY: all install uninstall clean

all:
	cargo build --release

install: all
	install -Dm755 target/release/$(BINARY) $(INSTALL_BIN)
	install -Dm644 $(SERVICE) $(INSTALL_SVC)
	systemctl --user daemon-reload
	@echo ""
	@echo "Installed $(BINARY) to $(INSTALL_BIN)"
	@echo "To enable autostart: systemctl --user enable --now hypr-overlay"

uninstall:
	systemctl --user disable --now hypr-overlay 2>/dev/null || true
	rm -f $(INSTALL_BIN) $(INSTALL_SVC)
	systemctl --user daemon-reload
	@echo "Uninstalled $(BINARY)"

clean:
	cargo clean
