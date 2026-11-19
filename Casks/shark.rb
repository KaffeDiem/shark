cask "shark" do
  version "0.1.51"
  sha256 "889b3cd0b46729e55be6075ed7eb66283c1c7f7d362d2ad2928419fb7e24e1fd"

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
