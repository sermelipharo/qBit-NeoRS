//! "Run external program" on torrent added / finished
//! (`Application::runExternalProgram`, non-Windows path).
//!
//! No shell is involved: the template is split into arguments first and the
//! placeholders are substituted inside each argument, so a torrent name such
//! as `x; rm -rf ~` stays one literal argument.

/// Values for the `%N %L %G %F %R %D %C %Z %T %I %J %K %M` placeholders.
#[derive(Clone, Debug, Default)]
pub struct AutorunVars {
    pub name: String,
    pub category: String,
    pub tags: Vec<String>,
    pub content_path: String,
    pub root_path: String,
    pub save_path: String,
    pub files_count: i64,
    pub total_size: i64,
    pub current_tracker: String,
    pub info_hash_v1: String,
    pub info_hash_v2: String,
    pub id: String,
    pub comment: String,
}

/// `Utils::String::splitCommand`: split on spaces outside double quotes;
/// quotes are kept in the argument.
pub fn split_command(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_quotes = false;
    let mut tmp = String::new();
    for c in command.chars() {
        if c == ' ' {
            if !in_quotes {
                if !tmp.is_empty() {
                    out.push(std::mem::take(&mut tmp));
                }
                continue;
            }
        } else if c == '"' {
            in_quotes = !in_quotes;
        }
        tmp.push(c);
    }
    if !tmp.is_empty() {
        out.push(tmp);
    }
    out
}

/// Placeholder substitution, scanning right to left and skipping the
/// character before a replacement so `%%N` keeps the first `%`.
pub fn replace_vars(s: &str, v: &AutorunVars) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    if chars.len() < 2 {
        return s.to_string();
    }
    let mut i = chars.len() as isize - 2;
    while i >= 0 {
        let idx = i as usize;
        if chars[idx] == '%' {
            let replacement = match chars[idx + 1] {
                'C' => Some(v.files_count.to_string()),
                'D' => Some(v.save_path.clone()),
                'F' => Some(v.content_path.clone()),
                'G' => Some(v.tags.join(",")),
                'I' => Some(if v.info_hash_v1.is_empty() { "-".to_string() } else { v.info_hash_v1.clone() }),
                'J' => Some(if v.info_hash_v2.is_empty() { "-".to_string() } else { v.info_hash_v2.clone() }),
                'K' => Some(v.id.clone()),
                'L' => Some(v.category.clone()),
                'M' => Some(v.comment.clone()),
                'N' => Some(v.name.clone()),
                'R' => Some(v.root_path.clone()),
                'T' => Some(v.current_tracker.clone()),
                'Z' => Some(v.total_size.to_string()),
                _ => None,
            };
            if let Some(r) = replacement {
                chars.splice(idx..idx + 2, r.chars());
            }
            i -= 1;
        }
        i -= 1;
    }
    chars.into_iter().collect()
}

/// Program + arguments for a template.
pub fn build_argv(template: &str, v: &AutorunVars) -> Vec<String> {
    split_command(template.trim())
        .into_iter()
        .map(|arg| {
            let stripped = if arg.len() >= 2 && arg.starts_with('"') && arg.ends_with('"') {
                arg[1..arg.len() - 1].to_string()
            } else {
                arg
            };
            replace_vars(&stripped, v)
        })
        .collect()
}

/// Starts the program detached (stdio to /dev/null) and reaps it in the
/// background. Returns false if it could not be started.
pub fn spawn(argv: &[String]) -> bool {
    let Some((program, args)) = argv.split_first() else { return false };
    if program.is_empty() {
        return false;
    }
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(false);
    match cmd.spawn() {
        Ok(mut child) => {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> AutorunVars {
        AutorunVars {
            name: "My Torrent; rm -rf ~".into(),
            category: "cat".into(),
            tags: vec!["a".into(), "b".into()],
            content_path: "/dl/My Torrent".into(),
            root_path: "/dl/My Torrent".into(),
            save_path: "/dl".into(),
            files_count: 3,
            total_size: 1234,
            current_tracker: "http://t".into(),
            info_hash_v1: "abc".into(),
            info_hash_v2: String::new(),
            id: "abc".into(),
            comment: "c".into(),
        }
    }

    #[test]
    fn splits_like_qbittorrent() {
        assert_eq!(split_command(r#"prog  a "b c" d"e f"g"#), vec!["prog", "a", "\"b c\"", "d\"e f\"g"]);
        assert_eq!(split_command("   "), Vec::<String>::new());
    }

    #[test]
    fn placeholders() {
        let v = vars();
        assert_eq!(replace_vars("%N|%L|%G|%C|%Z|%I|%J|%K|%D|%T|%M|%X", &v), "My Torrent; rm -rf ~|cat|a,b|3|1234|abc|-|abc|/dl|http://t|c|%X");
        assert_eq!(replace_vars("%%N", &v), "%My Torrent; rm -rf ~");
        assert_eq!(replace_vars("%", &v), "%");
        assert_eq!(replace_vars("100%", &v), "100%");
    }

    #[test]
    fn argv_keeps_values_in_one_argument() {
        let v = vars();
        let argv = build_argv(r#" /usr/bin/notify "%N" %F --size=%Z "" "#, &v);
        assert_eq!(argv, vec!["/usr/bin/notify", "My Torrent; rm -rf ~", "/dl/My Torrent", "--size=1234", ""]);
        let one_quote = build_argv("\"", &v);
        assert_eq!(one_quote, vec!["\""]);
    }
}
