fn words(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), '\\') => {
                if let Some(n) = chars.next() {
                    current.push(n);
                }
            }
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    current.push(n);
                }
            }
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            (None, ';' | '|' | '&' | '(' | ')' | '\n' | '`') => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                out.push(";".into());
            }
            (None, c) => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn simple_commands(command: &str) -> Vec<Vec<String>> {
    words(command)
        .split(|w| w == ";")
        .filter(|c| !c.is_empty())
        .map(|c| {
            let mut c: Vec<String> = c.to_vec();
            while let Some(first) = c.first() {
                let wrapper = matches!(first.as_str(), "sudo" | "doas" | "env" | "command" | "nohup" | "time" | "exec" | "xargs" | "nice")
                    || (first.contains('=') && !first.starts_with('-'));
                if !wrapper {
                    break;
                }
                c.remove(0);
                while c.first().is_some_and(|w| w.starts_with('-') && !w.starts_with("--")) {
                    c.remove(0);
                }
            }
            c
        })
        .filter(|c| !c.is_empty())
        .collect()
}

fn program(word: &str) -> &str {
    word.rsplit(['/', '\\']).next().unwrap_or(word)
}

fn has_flag(args: &[String], long: &[&str], short: char) -> bool {
    args.iter().any(|a| long.contains(&a.as_str()) || (a.starts_with('-') && !a.starts_with("--") && a.contains(short)))
}

fn git(args: &[String]) -> Option<String> {
    let mut rest: Vec<&String> = args.iter().collect();
    while rest.first().is_some_and(|a| a.starts_with('-')) {
        let flag = rest.remove(0);
        if matches!(flag.as_str(), "-C" | "-c" | "--git-dir" | "--work-tree") && !rest.is_empty() {
            rest.remove(0);
        }
    }
    let sub = rest.first()?.as_str();
    let tail: Vec<String> = rest[1..].iter().map(|s| s.to_string()).collect();
    let any = |xs: &[&str]| tail.iter().any(|a| xs.contains(&a.as_str()));
    Some(match sub {
        "reset" if any(&["--hard", "--merge", "--keep"]) => "git reset --hard throws away uncommitted changes".into(),
        "clean" if tail.iter().any(|a| a.starts_with('-') && (a.contains('f') || a == "--force")) => "git clean deletes untracked files".into(),
        "checkout" if any(&["--", "."]) || any(&["-f", "--force"]) => "git checkout over files throws away their uncommitted changes".into(),
        "restore" if !any(&["--staged", "-S"]) || any(&["--worktree", "-W"]) => "git restore throws away uncommitted changes".into(),
        "stash" if tail.first().is_none_or(|a| matches!(a.as_str(), "push" | "save" | "drop" | "clear" | "-u" | "--include-untracked" | "-a" | "--all")) => {
            "git stash moves or drops uncommitted work".into()
        }
        "branch" if any(&["-D", "-d", "--delete", "-M", "--move", "-f", "--force"]) => "git branch deletes or overwrites a branch".into(),
        "push" => "git push changes the shared remote".into(),
        "rebase" | "filter-branch" | "filter-repo" => format!("git {sub} rewrites history"),
        "commit" if any(&["--amend"]) => "git commit --amend rewrites the last commit".into(),
        "tag" if any(&["-d", "--delete", "-f", "--force"]) => "git tag deletes or moves a tag".into(),
        "update-ref" if any(&["-d"]) => "git update-ref -d deletes a ref".into(),
        "reflog" if any(&["expire", "delete"]) => "git reflog expire drops recovery points".into(),
        "worktree" if any(&["remove", "prune"]) => "git worktree remove deletes a worktree".into(),
        "rm" => "git rm deletes files".into(),
        _ => return None,
    })
}

fn scratch_only(args: &[String]) -> bool {
    let temp = std::env::temp_dir().to_string_lossy().into_owned();
    let targets: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    !targets.is_empty() && targets.iter().all(|a| !a.contains("..") && (a.starts_with("/tmp/") || a.starts_with("/private/tmp/") || a.starts_with(&temp)))
}

fn nested(name: &str, args: &[String]) -> Option<String> {
    match name {
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" => {
            let at = args.iter().position(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('c'))?;
            args.get(at + 1).cloned()
        }
        "eval" => (!args.is_empty()).then(|| args.join(" ")),
        _ => None,
    }
}

pub fn destructive(command: &str) -> Option<String> {
    destructive_at(command, 0)
}

fn destructive_at(command: &str, depth: usize) -> Option<String> {
    let lower = command.to_lowercase();
    for phrase in ["drop table", "drop database", "drop schema", "truncate table", "drop collection", "flushall", "flushdb"] {
        if lower.contains(phrase) {
            return Some(format!("the command contains `{phrase}`"));
        }
    }
    for cmd in simple_commands(command) {
        let name = program(&cmd[0]);
        let args = &cmd[1..];
        if let Some(inner) = nested(name, args) {
            if depth >= 4 {
                return Some(format!("`{name}` runs a command nested too deep to check"));
            }
            if let Some(reason) = destructive_at(&inner, depth + 1) {
                return Some(reason);
            }
            continue;
        }
        let reason = match name {
            "rm" | "rmdir" | "unlink" if scratch_only(args) => None,
            "rm" | "rmdir" | "unlink" | "shred" | "srm" | "trash" | "del" | "rd" | "erase" => Some(format!("`{name}` deletes files")),
            "remove-item" | "ri" => Some("Remove-Item deletes files".into()),
            "git" => git(args),
            "find" if args.iter().any(|a| a == "-delete") || args.windows(2).any(|w| w[0] == "-exec" && program(&w[1]) == "rm") => Some("find deletes the files it matches".into()),
            "dd" | "mkfs" | "diskutil" | "fdisk" | "parted" | "format" => Some(format!("`{name}` writes to disks")),
            "chmod" | "chown" if has_flag(args, &["--recursive"], 'R') => Some(format!("recursive `{name}` changes many files")),
            "mv" if args.iter().any(|a| a == "/" || a == "~" || a == "..") => Some("mv on a top-level folder".into()),
            "truncate" => Some("truncate empties files".into()),
            "docker" | "podman" if args.iter().any(|a| matches!(a.as_str(), "rm" | "rmi" | "prune" | "kill")) => Some(format!("`{name}` removes containers, images or volumes")),
            "kubectl" if args.first().is_some_and(|a| matches!(a.as_str(), "delete" | "drain" | "replace")) => Some("kubectl changes a cluster".into()),
            "npm" | "pnpm" | "yarn" | "bun" | "cargo" | "twine" | "gem" | "poetry" | "uv" if args.first().is_some_and(|a| a == "publish") => {
                Some(format!("`{name} publish` releases a package"))
            }
            "gh" if args.iter().any(|a| matches!(a.as_str(), "delete" | "merge" | "close")) => Some("gh changes GitHub".into()),
            "kill" | "pkill" | "killall" if args.iter().any(|a| a == "-9" || a == "-KILL") => Some(format!("`{name} -9` kills processes")),
            _ => None,
        };
        if reason.is_some() {
            return reason;
        }
    }
    None
}
