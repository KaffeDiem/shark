.PHONY: run test release

VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP := target/release/Shark.app
DMG := target/release/Shark-$(VERSION).dmg
# Ad-hoc by default; pass SIGN="Developer ID Application: ..." to sign for distribution.
SIGN ?= -
TIMESTAMP = $(if $(filter -,$(SIGN)),--timestamp=none,--timestamp)

run:
	cargo run

test:
	cargo test --features test-support

# Builds target/release/Shark-<version>.dmg (Shark.app + /Applications link).
release:
	cargo build --release
	rm -rf $(APP) $(DMG) target/release/dmg target/release/Shark.iconset
	mkdir -p $(APP)/Contents/MacOS $(APP)/Contents/Resources target/release/Shark.iconset target/release/dmg
	cp target/release/shark $(APP)/Contents/MacOS/shark
	for s in 16 32 128 256 512; do \
		sips -z $$s $$s assets/icon.png --out target/release/Shark.iconset/icon_$${s}x$${s}.png >/dev/null; \
		sips -z $$((s*2)) $$((s*2)) assets/icon.png --out target/release/Shark.iconset/icon_$${s}x$${s}@2x.png >/dev/null; \
	done
	iconutil -c icns target/release/Shark.iconset -o $(APP)/Contents/Resources/Shark.icns
	plutil -create xml1 $(APP)/Contents/Info.plist
	plutil -insert CFBundleName -string Shark $(APP)/Contents/Info.plist
	plutil -insert CFBundleDisplayName -string Shark $(APP)/Contents/Info.plist
	plutil -insert CFBundleIdentifier -string io.github.kaffediem.shark $(APP)/Contents/Info.plist
	plutil -insert CFBundleExecutable -string shark $(APP)/Contents/Info.plist
	plutil -insert CFBundleIconFile -string Shark $(APP)/Contents/Info.plist
	plutil -insert CFBundlePackageType -string APPL $(APP)/Contents/Info.plist
	plutil -insert CFBundleShortVersionString -string $(VERSION) $(APP)/Contents/Info.plist
	plutil -insert CFBundleVersion -string $(VERSION) $(APP)/Contents/Info.plist
	plutil -insert LSMinimumSystemVersion -string 11.0 $(APP)/Contents/Info.plist
	plutil -insert NSHighResolutionCapable -bool YES $(APP)/Contents/Info.plist
	plutil -insert NSHumanReadableCopyright -string "© Kasper Munch Jensen. GPL-3.0." $(APP)/Contents/Info.plist
	codesign --force --options runtime $(TIMESTAMP) --sign "$(SIGN)" $(APP)
	cp -R $(APP) target/release/dmg/
	ln -s /Applications target/release/dmg/Applications
	hdiutil create -volname Shark -srcfolder target/release/dmg -format UDZO -ov $(DMG)
	@echo "Built $(DMG)"
