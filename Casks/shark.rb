cask "shark" do
  version "0.1.10"
  sha256 "7053abb2b362c1f0e06cc1c28d535ec4eb539b826d0320d95c8f01c9bfa61286"

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
