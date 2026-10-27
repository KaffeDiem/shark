#!/bin/sh
# Rebuilds docs/images/*.png from a demo HOME (setup.sh), without touching your own Shark data.
#
#   scripts/screenshots/run.sh
#
# Drives the app with System Events keystrokes: the terminal (or app) running this needs
# Accessibility and Screen Recording access. Don't type while it runs.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=$root/docs/images
D=${SHARK_DEMO:-/tmp/shark-demo}
export SHARK_DEMO=$D

"$here/setup.sh"
cargo build --manifest-path "$root/Cargo.toml"
HOME=$D/home GIT_CONFIG_GLOBAL=/dev/null SHARK_GH="$here/gh" SHARK_CLAUDE=/usr/bin/false \
    "$root/target/debug/shark" >"$D/app.log" 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null' EXIT

win() { swift "$here/winid.swift" $pid | cut -d' ' -f1; }
while [ -z "$(win)" ]; do sleep 1; done
# key <key> [modifiers], e.g. key d "command down, shift down"; key code 53 is esc.
key() {
    using=""; [ -n "${2:-}" ] && using="using {$2}"
    osascript -e "tell application \"System Events\"
set frontmost of (first process whose unix id is $pid) to true
$1 $using
end tell" >/dev/null
    sleep 1.5
}
shot() { screencapture -o -x -l "$(win)" "$D/$1" && sips -Z 1600 "$D/$1" --out "$out/$1" >/dev/null; }

osascript -e "tell application \"System Events\" to tell (first process whose unix id is $pid) to set size of window 1 to {1440, 900}"
key 'keystroke "d"' "command down, shift down" # diff
key 'keystroke "b"' "command down, option down" # Workspace tools
sleep 2 # git status and the diff
shot hero.png
key 'keystroke "k"' "command down"
shot palette.png
key 'key code 53'
key 'key code 53'
key 'keystroke "b"' "command down, option down"
key 'keystroke "2"' "command down" # eng-118, its plan opens
shot plan-versions.png
key 'keystroke "4"' "command down" # eng-127, a changed image
key 'keystroke "d"' "command down, shift down"
sleep 2
shot diff-images.png
key 'keystroke "n"' "command down"
key 'keystroke "eng-131-sign-in-with-apple"'
shot multi-repo.png
key 'key code 53'
echo "Wrote $out"
