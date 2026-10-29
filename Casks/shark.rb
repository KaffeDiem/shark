cask "shark" do
  version "0.1.16"
  sha256 "30f411b7f1957e19e04ee0793b0d17e37d28b79edca147b9dcfcfdd49ae4aff2"

  url "https://github.com/KaffeDiem/shark/releases/download/v#{version}/Shark-#{version}.dmg"
  name "Shark"
  desc "Keyboard-first app for running Claude Code across several repos"
  homepage "https://github.com/KaffeDiem/shark"

  depends_on arch: :arm64
  depends_on :macos

  app "Shark.app"

  # The app is ad-hoc signed, not notarized, so Gatekeeper would block it.
  postflight_steps do
    run "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "{{appdir}}/Shark.app"]
  end
end
