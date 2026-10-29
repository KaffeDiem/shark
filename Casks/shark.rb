cask "shark" do
  version "0.1.15"
  sha256 "627c2247bdbcd82c5bc6b3fd09500da14165814c01de20757a8be1cf58c6c69c"

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
