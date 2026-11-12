cask "shark" do
  version "0.1.39"
  sha256 "6c049532d724892508b2077dbd88e18cea6b6fa0dc0b953a55e0d897490e9aca"

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
