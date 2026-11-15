//! Plan mode's read-only Bash: commands claude may run without asking the user.
//!
//! ponytail: a hand-rolled allowlist, not a shell parser. It reads quotes, escapes and the
//! command separators, and refuses any syntax it doesn't follow. Repo git config (fsmonitor,
//! pager) is trusted, because these are the user's own worktrees.

/// Commands that only read, whatever their arguments.
const PLAIN: &[&str] = &[
    "ls", "cat", "head", "tail", "grep", "wc", "cd", "pwd", "echo", "stat", "du", "diff", "cut",
    "tr", "basename", "dirname", "realpath", "which", "jq",
];
/// Git subcommands that only read (`branch`, `remote`, `worktree` and `stash` are in `git`).
const GIT: &[&str] = &[
    "log",
    "show",
    "diff",
    "status",
    "blame",
    "ls-files",
    "ls-tree",
    "rev-parse",
    "describe",
    "shortlog",
    "cat-file",
    "merge-base",
];

/// Whether `cmd` only reads: each of its commands is allowlisted, with no expansion, background
/// job or redirect other than to /dev/null. When in doubt, false.
pub(super) fn read_only(cmd: &str) -> bool {
    segments(cmd).is_some_and(|s| {
        s.iter().any(|(w, _)| !w.is_empty()) && s.iter().all(|(w, glob)| allowed(w, *glob))
    })
}

/// `cmd`'s commands, split on `&&`, `||`, `;`, `|` and newline, as their unquoted words and
/// whether any word has an unquoted glob. None for syntax outside the allowlist.
fn segments(cmd: &str) -> Option<Vec<(Vec<String>, bool)>> {
    let (mut segs, mut words, mut glob) = (vec![], vec![], false);
    // The word being read: Some once it starts, even as ''. `quoted` if any of it was.
    let (mut word, mut quoted) = (None::<String>, false);
    let mut cs = cmd.char_indices().peekable();
    while let Some((i, c)) = cs.next() {
        match c {
            ' ' | '\t' | '\n' | ';' | '|' | '&' | '>' => {}
            '<' | '(' | ')' | '{' | '}' | '$' | '`' => return None,
            '#' if word.is_none() => return None, // a comment, which can hide a quote
            '\'' => {
                quoted = true;
                let w = word.get_or_insert_default();
                loop {
                    match cs.next()?.1 {
                        '\'' => break,
                        c => w.push(c),
                    }
                }
                continue;
            }
            '"' => {
                quoted = true;
                let w = word.get_or_insert_default();
                loop {
                    match cs.next()?.1 {
                        '"' => break,
                        '$' | '`' => return None, // these still expand in double quotes
                        '\\' => match cs.next()?.1 {
                            c @ ('$' | '`' | '"' | '\\') => w.push(c),
                            '\n' => {}
                            c => w.extend(['\\', c]),
                        },
                        c => w.push(c),
                    }
                }
                continue;
            }
            '\\' => {
                quoted = true;
                match cs.next()?.1 {
                    '\n' => {} // a line continuation
                    c => word.get_or_insert_default().push(c),
                }
                continue;
            }
            c => {
                glob |= matches!(c, '*' | '?' | '[');
                word.get_or_insert_default().push(c);
                continue;
            }
        }
        // c ends the word.
        if c == '>' && !quoted && matches!(word.as_deref(), Some("1" | "2")) {
            word = None; // the fd it redirects
        }
        words.extend(word.take());
        quoted = false;
        match c {
            ' ' | '\t' => continue,
            '>' => {
                let rest = &cmd[i + 1..];
                let after = rest.strip_prefix("&1").or_else(|| {
                    rest.trim_start_matches([' ', '\t'])
                        .strip_prefix("/dev/null")
                })?;
                if !after
                    .chars()
                    .next()
                    .is_none_or(|n| " \t\n;|&<>()".contains(n))
                {
                    return None; // `>/dev/nullx` or `>>`
                }
                let end = cmd.len() - after.len();
                while cs.next_if(|&(j, _)| j < end).is_some() {}
                continue;
            }
            '&' if cs.next_if(|&(_, n)| n == '&').is_none() => return None, // a background job
            '|' => drop(cs.next_if(|&(_, n)| n == '|')),
            _ => {} // ;, newline or &&
        }
        segs.push((std::mem::take(&mut words), std::mem::take(&mut glob)));
    }
    words.extend(word);
    segs.push((words, glob));
    Some(segs)
}

/// Whether one command only reads. `glob` refuses the commands with risky flags, since a glob
/// can expand to a file named like one (`--pre=sh`).
fn allowed(words: &[String], glob: bool) -> bool {
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    let Some((&cmd, args)) = w.split_first() else {
        return true;
    };
    let none = |bad: fn(&str) -> bool| !glob && !args.iter().any(|a| bad(a));
    match cmd {
        // uniq's second operand is a file it writes, and after `--` even `-x` is an operand.
        "sort" | "uniq" => none(|a| {
            !a.starts_with('-')
                || a == "-"
                || a == "--"
                || (!a.starts_with("--") && a.contains('o'))
                || long(a, "output")
                || long(a, "compress-program")
        }),
        "find" => none(|a| {
            ["-exec", "-ok", "-fprint"].iter().any(|p| a.starts_with(p))
                || a == "-delete"
                || a == "-fls"
        }),
        "rg" => none(|a| a.starts_with("--pre") || a.starts_with("--hostname-bin")),
        "git" => !glob && git(args),
        _ => PLAIN.contains(&cmd),
    }
}

fn git(args: &[&str]) -> bool {
    let risky = |a: &&str| {
        *a == "-c"
            || ["output", "ext-diff", "textconv", "filters"]
                .iter()
                .any(|o| long(a, o))
    };
    if args.iter().any(risky) {
        return false;
    }
    let mut a = args;
    loop {
        match a {
            ["--no-pager" | "-P", rest @ ..] | ["-C", _, rest @ ..] => a = rest,
            _ => break,
        }
    }
    match a {
        ["branch", flags @ ..] => flags
            .iter()
            .all(|f| ["-a", "-r", "-v", "--list"].contains(f)),
        ["remote", "-v"] | ["worktree" | "stash", "list", ..] => true,
        [sub, ..] => GIT.contains(sub),
        [] => false,
    }
}

/// Whether `arg` is long option `opt`, an abbreviation of it (getopt and git take those), or
/// `opt` with more on the end (`--output-indicator-new`).
fn long(arg: &str, opt: &str) -> bool {
    let name = arg.strip_prefix("--").and_then(|a| a.split('=').next());
    name.is_some_and(|n| !n.is_empty() && (opt.starts_with(n) || n.starts_with(opt)))
}

#[cfg(test)]
mod tests {
    use super::read_only;

    #[test]
    fn allowlist() {
        for (cmd, ok) in [
            ("cd shark && git log --oneline -5", true),
            ("git --no-pager diff", true),
            ("grep -rn foo src | sort | uniq -c | head -20", true),
            ("grep -rn 'fn on_permission(' src", true),
            ("grep '->' f", true),
            ("grep 'a|b$' f", true),
            ("find . -name '*.rs'", true),
            ("git -C shark diff", true),
            ("ls 2>/dev/null", true),
            ("ls src 2>&1 | head", true),
            ("ls 2> /dev/null; pwd\n", true),
            ("git -P -C a --no-pager status -s || git branch -a -v", true),
            ("git worktree list --porcelain && git stash list", true),
            ("git remote -v", true),
            ("cat \"a b\" 'c' d\\ e", true),
            ("grep \"\\$x\" f", true),
            ("sort -rn -k2 | uniq -c", true),
            ("rm x", false),
            ("ls & rm x", false),
            ("ls &", false),
            ("ls > f", false),
            ("ls >/dev/null2", false),
            ("ls >>/dev/null", false),
            ("ls &>/dev/null", false),
            ("ls 2>&1x", false),
            ("echo $(rm x)", false),
            ("echo \"$(rm x)\"", false),
            ("echo `id`", false),
            ("echo \"`id`\"", false),
            ("echo $HOME", false),
            ("cat <f", false),
            ("cat <<EOF", false),
            ("(rm x)", false),
            ("{ rm x; }", false),
            ("ls\nrm x", false),
            ("ls; rm x", false),
            ("ls | rm x", false),
            ("ls |& rm x", false),
            ("ls #'\nrm x\n#'", false),
            ("ls '", false),
            ("echo \"", false),
            ("", false),
            ("find . -delete", false),
            ("find . -exec rm {} +", false),
            ("find . -execdir rm \\;", false),
            ("find . -fprint f", false),
            ("find * -name x", false),
            ("git -c core.pager=x log", false),
            ("git log -c", false),
            ("git grep -Orm foo", false),
            ("git branch new", false),
            ("git branch -D x", false),
            ("git push", false),
            ("git stash", false),
            ("git remote add x y", false),
            ("git worktree add x", false),
            ("git log --output=f", false),
            ("git diff --out=f", false),
            ("git diff --ext-diff", false),
            ("git show --textconv x", false),
            ("git cat-file --filters x", false),
            ("git --exec-path=x log", false),
            ("git log -- *.rs", false),
            ("uniq /dev/null f", false),
            ("uniq -c f", false),
            ("echo x | uniq -- - -x", false),
            ("uniq - -x", false),
            ("sort -o f x", false),
            ("sort -ro f", false),
            ("sort --out=f", false),
            ("sort --compress-program=sh x", false),
            ("sort --compress=sh", false),
            ("rg --pre sh x", false),
            ("rg --pre-glob '*' x", false),
            ("rg --hostname-bin=sh x", false),
            ("rg foo *", false),
            ("file -C -m f", false),
            ("FOO=1 ls", false),
            ("/bin/rm x", false),
            ("xargs rm", false),
            ("sed -i s/a/b/ f", false),
        ] {
            assert_eq!(read_only(cmd), ok, "{cmd:?}");
        }
    }
}
