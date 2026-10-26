#!/bin/sh
# A repo reproducing the design's Changes example (+5 −3 over 3 files), for screenshots.
#
#   scripts/demo-repo.sh [dir]      # default: a new temp dir; prints the repo path
#
# Then in Shark: cmd-o and pick the printed `shark` repo, cmd-n, name the branch
# `better-diff`, enter. The workspace starts on the pushed `better-diff` branch, so
# Changes shows "origin/master → working tree" with the three files.
set -eu
root=${1:-$(mktemp -d -t shark-demo)}
if [ -e "$root/shark" ]; then echo "$root/shark already exists" >&2; exit 1; fi
mkdir -p "$root/shark/src/app"
root=$(cd "$root" && pwd)
repo=$root/shark
cd "$repo"
git init -q -b master
git config user.name demo
git config user.email demo@example.com

lines() { i=1; while [ "$i" -le "$1" ]; do echo "// line $i"; i=$((i + 1)); done; }
app() {
    lines 629
    cat <<EOF
            resizable_panel()
                .size(px($1.))
                .size_range(px(320.)..px(900.))
                .child(chat_view),
            resizable_panel()
                .child(self.render_editor(cx)),
EOF
}
chat() {
    lines 395
    echo '        Input::new(&self.input)'
    printf '%s\n' "$@"
    echo '            .w_full()'
    echo '            .into_any_element()'
}

app 420 >src/app.rs
printf 'fn sidebar() {}\nconst WIDTH: f32 = 240.;\n' >src/app/sidebar.rs
chat '            .placeholder("Message the agent")' >src/chat.rs
git add -A && git commit -qm base

git checkout -qb better-diff
app 360 >src/app.rs
printf 'fn sidebar() {}\nconst WIDTH: f32 = 220.;\n' >src/app/sidebar.rs
chat '            .placeholder(' '                "Message the agent (@ files, / skills)"' '            )' >src/chat.rs
git commit -qam "demo changes"
git checkout -q master

git init -q --bare "$root/origin.git"
git remote add origin "$root/origin.git"
git push -q origin master better-diff
git remote set-head origin master
echo "$repo"
