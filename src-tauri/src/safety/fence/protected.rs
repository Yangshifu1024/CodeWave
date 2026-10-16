//! Static protection of explicit internal-state write targets, before approval policy.
//! This is not a shell sandbox: variable expansion, arbitrary scripts and external
//! programs can have effects that are not inferable from command-line syntax.
use super::*;

pub fn check_protected_command(cmd: &str, cwd: &Path, roots: &WriteRoots) -> Verdict {
    let mut scan = Scan {
        roots,
        cwds: vec![pathutil::canonical_best_effort(cwd)],
        blocked: None,
        depth: 0,
    };
    scan.script(cmd);
    scan.blocked.unwrap_or(Verdict::Allow)
}

struct Scan<'a> {
    roots: &'a WriteRoots,
    // Preserve earlier locations too: conditional branches need not execute cd.
    // A bounded superset avoids claiming that a single AST traversal models shell control flow.
    cwds: Vec<PathBuf>,
    blocked: Option<Verdict>,
    depth: usize,
}

impl Scan<'_> {
    fn block(&mut self, message: impl Into<String>) {
        self.blocked = Some(Verdict::Block {
            code: "E_INTERNAL_STATE".into(),
            message: message.into(),
        });
    }

    fn script(&mut self, text: &str) {
        if self.depth >= 8 {
            self.block("嵌套 shell 超过内部状态静态检查上限，请拆分命令");
            return;
        }
        let parsed = parse_grammar(text, tree_sitter_bash::LANGUAGE.into())
            .filter(|tree| !tree.root_node().has_error())
            .map(|tree| (tree, false))
            .or_else(|| {
                parse_grammar(text, tree_sitter_powershell::LANGUAGE.into())
                    .filter(|tree| !tree.root_node().has_error())
                    .map(|tree| (tree, true))
            })
            // Even a partially parsed script can expose literal commands/redirects.
            .or_else(|| {
                parse_grammar(text, tree_sitter_bash::LANGUAGE.into()).map(|tree| (tree, false))
            });
        if let Some((tree, ps)) = parsed {
            self.walk(tree.root_node(), text.as_bytes(), ps);
        }
    }

    fn walk(&mut self, node: tree_sitter::Node, src: &[u8], ps: bool) {
        if self.blocked.is_some() {
            return;
        }
        // Directory changes inside subshells do not escape into their caller.
        if matches!(node.kind(), "subshell" | "command_substitution") {
            let saved = self.cwds.clone();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                self.walk(child, src, ps);
            }
            self.cwds = saved;
            return;
        }
        if node.kind() == "command" {
            let name_node = if ps {
                node.children(&mut node.walk())
                    .find(|child| matches!(child.kind(), "command_name" | "command_name_expr"))
            } else {
                node.child_by_field_name("name")
            };
            let name = name_node
                .and_then(|name| name.utf8_text(src).ok())
                .map(deobfuscate_name)
                .unwrap_or_default();
            let mut cursor = node.walk();
            let args: Vec<_> = node
                .children(&mut cursor)
                .filter(|child| Some(*child) != name_node)
                .filter(|child| {
                    if ps {
                        matches!(
                            child.kind(),
                            "command_parameter"
                                | "generic_token"
                                | "variable"
                                | "string_literal"
                                | "expandable_string_literal"
                        )
                    } else {
                        matches!(
                            child.kind(),
                            "word" | "string" | "raw_string" | "concatenation" | "number"
                        )
                    }
                })
                .map(|child| unquote(child.utf8_text(src).unwrap_or_default()).to_owned())
                .collect();
            self.command(&name, &args);
        }
        if matches!(node.kind(), "file_redirect" | "redirection") {
            let destination = if ps {
                node.children(&mut node.walk())
                    .find(|child| child.kind() == "redirected_file_name")
            } else {
                node.child_by_field_name("destination")
            };
            if let Some(destination) = destination {
                let raw = destination.utf8_text(src).unwrap_or_default();
                let text = node.utf8_text(src).unwrap_or_default();
                let fd_dup = raw.chars().all(|ch| ch.is_ascii_digit()) && text.contains('&');
                // Input redirection reads a file; it must not be classified as a write.
                if !fd_dup && text.contains('>') {
                    self.target(raw);
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk(child, src, ps);
        }
    }

    fn nested(&mut self, script: &str) {
        let mut child = Scan {
            roots: self.roots,
            cwds: self.cwds.clone(),
            blocked: None,
            depth: self.depth + 1,
        };
        child.script(script);
        if child.blocked.is_some() {
            self.blocked = child.blocked;
        }
    }

    fn nested_arguments(&mut self, arguments: &[String]) {
        if let [script] = arguments {
            self.nested(script);
        } else {
            // PS/CMD can combine script fragments across argument boundaries.
            // Inspect that projection before restoring quoted literal arguments.
            let raw_script = arguments.join(" ");
            self.nested(&raw_script);
            if self.blocked.is_some() {
                return;
            }
            // A fragment can carry syntax ("x >") while another argument is a
            // path containing spaces. Preserve those path boundaries without
            // turning the fragment's operators back into literal text.
            let fragments = arguments
                .iter()
                .map(|argument| {
                    if argument.contains(['\'', '"', ';', '&', '|', '<', '>', '(', ')']) {
                        argument.clone()
                    } else {
                        quote_nested_argument(argument)
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            if fragments != raw_script {
                self.nested(&fragments);
                if self.blocked.is_some() {
                    return;
                }
            }
            // walk removed each argument's outer quotes. Restore boundaries for
            // command syntaxes that interpret all remaining arguments as script.
            let script = arguments
                .iter()
                .map(|argument| quote_nested_argument(argument))
                .collect::<Vec<_>>()
                .join(" ");
            if script != raw_script && script != fragments {
                self.nested(&script);
            }
        }
    }

    fn paths(&self, raw: &str) -> Vec<PathBuf> {
        let raw = unquote(raw);
        if raw.is_empty()
            || raw == "-"
            || raw.eq_ignore_ascii_case("$null")
            || raw == "/dev/null"
            || pathutil::strip_verbatim_prefix(raw).contains(['$', '`', '*', '?'])
        {
            return Vec::new();
        }
        let path = if let Some(relative) = raw.strip_prefix("~/") {
            let Some(home) = dirs::home_dir() else {
                return Vec::new();
            };
            home.join(relative)
        } else {
            PathBuf::from(raw)
        };
        if path.is_absolute() {
            vec![path]
        } else {
            self.cwds
                .iter()
                // Keep symlink/.. traversal intact; lexical normalization can point
                // somewhere different from the path the filesystem actually opens.
                .map(|cwd| cwd.join(&path))
                .collect()
        }
    }

    fn target(&mut self, raw: &str) {
        self.target_kind(raw, false);
    }

    fn target_kind(&mut self, raw: &str, direct: bool) {
        for path in self.paths(raw) {
            let result = if direct {
                crate::core::protected_state::check_direct_write(self.roots, &path)
            } else {
                crate::core::protected_state::check_write(self.roots, &path)
            };
            if let Err((_, message)) = result {
                self.block(message);
                return;
            }
        }
    }

    fn command(&mut self, raw_name: &str, args: &[String]) {
        let name = raw_name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(raw_name)
            .trim_end_matches(".exe");
        if TRANSPARENT.contains(&name) {
            if let Ok(Some((name, args))) = strip_transparency(name, args) {
                self.command(&name, &args);
            }
            return;
        }
        if matches!(name, "sh" | "bash" | "dash" | "zsh" | "powershell" | "pwsh") {
            let powershell = matches!(name, "powershell" | "pwsh");
            let command = args
                .iter()
                .take_while(|arg| arg.as_str() != "--")
                .position(|arg| {
                    if powershell {
                        arg.eq_ignore_ascii_case("-c") || arg.eq_ignore_ascii_case("-command")
                    } else {
                        arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains('c')
                    }
                });
            if let Some(index) = command
                && let Some(script) = args.get(index + 1)
            {
                if powershell {
                    self.nested_arguments(&args[index + 1..]);
                } else {
                    self.nested(script);
                } // Remaining shell argv are data, not additional code.
            }
            return;
        }
        if CMD_HOSTS.contains(&name) {
            if let Some(index) = args
                .iter()
                .position(|arg| matches!(arg.to_ascii_lowercase().as_str(), "/c" | "/k"))
            {
                self.nested_arguments(&args[index + 1..]);
            }
            return;
        }
        if WSL_HOSTS.contains(&name) {
            if let Some((inner, shell)) = extract_wsl_inner(args) {
                if shell {
                    self.nested(&inner);
                } else if let Some((name, args)) = inner.split_once(' ') {
                    self.command(
                        name,
                        &args
                            .split_whitespace()
                            .map(str::to_owned)
                            .collect::<Vec<_>>(),
                    );
                }
            }
            return;
        }
        if matches!(
            name,
            "cd" | "chdir" | "set-location" | "sl" | "pushd" | "push-location"
        ) {
            let destination = option_values(args, &["-path", "-literalpath"])
                .into_iter()
                .next()
                .or_else(|| positionals(args, &[]).first().cloned());
            if let Some(destination) = destination {
                for path in self.paths(&destination) {
                    let path = pathutil::canonical_best_effort(&path);
                    if !self.cwds.contains(&path) {
                        self.cwds.push(path);
                    }
                    if self.cwds.len() > 64 {
                        self.block("目录分支超过内部状态静态检查上限，请拆分命令");
                        return;
                    }
                }
            }
            return;
        }
        if DELETE_CMDS.contains(&name) {
            for target in option_values(args, &["-path", "-literalpath"])
                .into_iter()
                .chain(positionals(args, &["-path", "-literalpath"]))
            {
                self.target(&target);
            }
            return;
        }
        if name == "mklink" {
            // cmd's slash switches must not be mistaken for a link/source path.
            for path in args.iter().filter(|argument| {
                !matches!(argument.to_ascii_lowercase().as_str(), "/h" | "/d" | "/j")
            }) {
                self.target(path);
            }
            return;
        }
        if name == "fsutil"
            && args
                .first()
                .is_some_and(|arg| arg.eq_ignore_ascii_case("hardlink"))
            && args
                .get(1)
                .is_some_and(|arg| arg.eq_ignore_ascii_case("create"))
        {
            for path in args.iter().skip(2).take(2) {
                self.target(path);
            }
            return;
        }
        let moving = matches!(
            name,
            "mv" | "move" | "mi" | "move-item" | "ren" | "rename" | "rename-item"
        );
        let copying = matches!(
            name,
            "cp" | "copy" | "cpi" | "copy-item" | "rsync" | "ln" | "link"
        );
        if moving || copying {
            let named = option_values(
                args,
                &[
                    "-destination",
                    "-target",
                    "-t",
                    "--target-directory",
                    "-newname",
                ],
            );
            let positional = positionals(
                args,
                &[
                    "-destination",
                    "-target",
                    "-t",
                    "--target-directory",
                    "-newname",
                    "-path",
                    "-literalpath",
                ],
            );
            let mut sources = option_values(args, &["-path", "-literalpath"]);
            let source_end = if named.is_empty() {
                positional.len().saturating_sub(1)
            } else {
                positional.len()
            };
            sources.extend_from_slice(&positional[..source_end]);
            let link_source = matches!(name, "ln" | "link")
                || (name == "cp"
                    && args.iter().any(|arg| {
                        arg == "--link"
                            || arg.starts_with("--link=")
                            || arg == "--symbolic-link"
                            || arg.starts_with("--symbolic-link=")
                            || (arg.starts_with('-')
                                && !arg.starts_with("--")
                                && arg[1..].contains(['l', 's']))
                    }));
            let remove_sources = name == "rsync"
                && args.iter().any(|arg| {
                    matches!(
                        arg.as_str(),
                        "--remove-source-files" | "--remove-sent-files"
                    )
                });
            if moving || link_source || remove_sources {
                for source in &sources {
                    self.target(source);
                }
            }
            let destinations = if named.is_empty() {
                positional.last().cloned().into_iter().collect()
            } else {
                named
            };
            let recursive_contents = name == "rsync"
                && (args.iter().any(|arg| arg.starts_with("--delete"))
                    || sources.iter().any(|source| source.ends_with('/')));
            let replace_container = name == "cp"
                && args.iter().any(|arg| {
                    arg == "--no-target-directory"
                        || (arg.starts_with('-')
                            && !arg.starts_with("--")
                            && arg[1..].contains('T'))
                });
            for destination in destinations {
                if recursive_contents || replace_container {
                    self.target(&destination);
                } else {
                    self.copy_destination(&destination, &sources);
                }
            }
            return;
        }
        if matches!(
            name,
            "set-content" | "add-content" | "out-file" | "tee-object" | "new-item" | "sc" | "ac"
        ) {
            if name == "new-item"
                && option_values(args, &["-itemtype"]).iter().any(|kind| {
                    kind.eq_ignore_ascii_case("hardlink")
                        || kind.eq_ignore_ascii_case("symboliclink")
                        || kind.eq_ignore_ascii_case("junction")
                })
            {
                for target in option_values(args, &["-target", "-value"]) {
                    self.target(&target);
                }
            }
            let named = option_values(args, &["-path", "-literalpath", "-filepath"]);
            if named.is_empty() {
                if let Some(destination) =
                    positionals(args, &["-value", "-encoding", "-itemtype"]).first()
                {
                    self.target(destination);
                }
            } else {
                for destination in named {
                    self.target(&destination);
                }
            }
            return;
        }
        if WRITE_ALL.contains(&name) {
            for argument in positionals(args, &[]) {
                if name == "dd" {
                    if let Some(path) = argument.strip_prefix("of=") {
                        self.target(path);
                    }
                } else {
                    self.target_kind(&argument, matches!(name, "touch" | "mkdir" | "md"));
                }
            }
        }
        if matches!(name, "chmod" | "chown" | "chgrp") {
            for path in positionals(args, &[]).into_iter().skip(1) {
                self.target(&path);
            }
        }
        if name == "sed" && args.iter().any(|arg| sed_in_place_hit(arg)) {
            // Only existing literal source files are mutated implicitly; script expressions
            // are not reclassified as filenames merely because they contain a protected name.
            for path in positionals(args, &["-e", "--expression", "-f", "--file"]) {
                if self.paths(&path).iter().any(|path| path.exists()) {
                    self.target(&path);
                }
            }
        }
        if IMPLICIT_WRITE_CMDS.contains(&name) && !implicit_write_exempt(args) {
            let output = option_values(args, &["-o", "--output"]);
            for path in &output {
                self.target(path);
            }
            let suffix = option_values(args, &["-S", "--suffix"]).into_iter().next();
            for path in positionals(
                args,
                &[
                    "-o",
                    "--output",
                    "-S",
                    "--suffix",
                    "-D",
                    "--dictID",
                    "-T",
                    "--threads",
                ],
            ) {
                self.target(&path);
                if output.is_empty()
                    && let Some(output) = compression_output(name, args, &path, suffix.as_deref())
                {
                    self.target(&output);
                }
            }
        }
        if name == "tar"
            && args.iter().any(|arg| {
                matches!(
                    tar_arg_op(arg),
                    Some(TarOp::Create | TarOp::Modify | TarOp::Delete)
                )
            })
            && let Some(path) = tar_file_value(args)
        {
            self.target(&path);
        }
        let extracts = (name == "tar"
            && args
                .iter()
                .any(|arg| tar_arg_op(arg) == Some(TarOp::Extract)))
            || (name == "unzip" && !args.iter().any(|arg| unzip_readonly_form(arg)))
            || (name == "cpio" && args.iter().any(|arg| cpio_extract_hit(arg)));
        if extracts && !archive_stdout_only(name, args) {
            let flags: &[&str] = if name == "tar" {
                &["-C", "--directory"]
            } else if name == "unzip" {
                &["-d"]
            } else {
                &["-D", "--directory"]
            };
            let destinations = option_values(args, flags);
            if destinations.is_empty() {
                self.target(".");
            } else {
                for destination in destinations {
                    self.target(&destination);
                }
            }
        }
        if DOWNLOAD_CMDS.contains(&name) {
            for path in download_write_targets(args) {
                self.target(&path);
            }
        }
    }

    fn copy_destination(&mut self, destination: &str, sources: &[String]) {
        for destination in self.paths(destination) {
            let children: Vec<_> = sources
                .iter()
                .filter_map(|source| {
                    let path = Path::new(unquote(source));
                    // cp dir/. destination copies contents; leave that ambiguous container strict.
                    if source.ends_with("/.") || source.ends_with("\\.") {
                        None
                    } else {
                        path.file_name().map(|name| destination.join(name))
                    }
                })
                .collect();
            let targets = if destination.is_dir()
                && children.len() == sources.len()
                && !children.is_empty()
            {
                children
            } else {
                vec![destination]
            };
            for path in targets {
                if let Err((_, message)) =
                    crate::core::protected_state::check_write(self.roots, &path)
                {
                    self.block(message);
                    return;
                }
            }
        }
    }
}

fn unquote(raw: &str) -> &str {
    let raw = raw.trim();
    raw.strip_prefix('"')
        .and_then(|raw| raw.strip_suffix('"'))
        .or_else(|| {
            raw.strip_prefix('\'')
                .and_then(|raw| raw.strip_suffix('\''))
        })
        .unwrap_or(raw)
}

fn quote_nested_argument(argument: &str) -> String {
    if !argument.contains(|ch: char| ch.is_whitespace() || "'\";&|<>()".contains(ch)) {
        return argument.to_owned();
    }
    if !argument.contains('\'') {
        format!("'{argument}'")
    } else {
        format!("\"{}\"", argument.replace('"', "\\\""))
    }
}

/// Parse only known stdout-only forms. A flag value is data, even when it
/// contains O/p/c; neither attached values nor operands after -- grant an exemption.
fn archive_stdout_only(name: &str, args: &[String]) -> bool {
    if !matches!(name, "tar" | "unzip") {
        return false;
    }
    let mut stdout = false;
    let mut skip_value = false;
    for arg in args {
        if skip_value {
            skip_value = false;
            continue;
        }
        if arg == "--" {
            break;
        }
        if let Some(flag) = arg.strip_prefix("--") {
            if name != "tar" {
                return false;
            }
            let (flag, attached) = flag
                .split_once('=')
                .map_or((flag, false), |(flag, _)| (flag, true));
            if flag == "to-stdout" && !attached {
                stdout = true;
            } else if matches!(
                flag,
                "file"
                    | "directory"
                    | "files-from"
                    | "exclude-from"
                    | "exclude"
                    | "format"
                    | "blocking-factor"
                    | "newer"
                    | "newer-mtime"
                    | "listed-incremental"
                    | "starting-file"
                    | "label"
                    | "strip-components"
                    | "transform"
                    | "use-compress-program"
            ) {
                skip_value = !attached;
            } else if !matches!(
                flag,
                "extract"
                    | "get"
                    | "list"
                    | "verbose"
                    | "gzip"
                    | "gunzip"
                    | "bzip2"
                    | "xz"
                    | "lzma"
                    | "zstd"
                    | "no-same-owner"
                    | "no-same-permissions"
                    | "numeric-owner"
                    | "absolute-names"
                    | "keep-old-files"
                    | "overwrite"
            ) {
                return false;
            }
            continue;
        }
        let Some(cluster) = arg.strip_prefix('-').filter(|cluster| !cluster.is_empty()) else {
            continue;
        };
        let mut flags = cluster.chars().peekable();
        while let Some(flag) = flags.next() {
            let takes_value = if name == "tar" {
                "fCXTbHFINgKVL".contains(flag)
            } else {
                "dP".contains(flag)
            };
            if takes_value {
                skip_value = flags.peek().is_none();
                break;
            }
            if (name == "tar" && flag == 'O') || (name == "unzip" && matches!(flag, 'p' | 'c')) {
                stdout = true;
            } else if !(if name == "tar" {
                "xtvzjJpmhPkwosi"
            } else {
                "abfuonqvltzjLsCVXKDU"
            })
            .contains(flag)
            {
                // Unknown options can change interpretation or consume a value.
                return false;
            }
        }
    }
    stdout && !skip_value
}

/// Common deterministic suffix transformations only; archive member names and
/// program-specific scripts remain outside this static command-line model.
fn compression_output(
    name: &str,
    args: &[String],
    input: &str,
    suffix: Option<&str>,
) -> Option<String> {
    let default_suffix = match name {
        "gzip" | "gunzip" => ".gz",
        "bzip2" | "bunzip2" => ".bz2",
        "xz" | "unxz" => ".xz",
        "zstd" | "unzstd" => ".zst",
        "lz4" | "unlz4" => ".lz4",
        "compress" | "uncompress" => ".Z",
        "pack" | "unpack" => ".z",
        _ => return None,
    };
    let decompress = matches!(
        name,
        "gunzip" | "bunzip2" | "unxz" | "unzstd" | "unlz4" | "uncompress" | "unpack"
    ) || args.iter().any(|arg| {
        matches!(arg.as_str(), "--decompress" | "--uncompress")
            || (arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains('d'))
    });
    let suffix = suffix.unwrap_or(default_suffix);
    if decompress {
        input.strip_suffix(suffix).map(str::to_owned)
    } else {
        Some(format!("{input}{suffix}"))
    }
}

fn option_values(args: &[String], flags: &[&str]) -> Vec<String> {
    let mut result = Vec::new();
    for (index, argument) in args.iter().enumerate() {
        let lower = argument.to_ascii_lowercase();
        for flag in flags {
            // POSIX short flags are case sensitive: cp -T must not become -t.
            let comparable = if flag.len() == 2 {
                argument.as_str()
            } else {
                lower.as_str()
            };
            if comparable == *flag {
                if let Some(value) = args.get(index + 1) {
                    result.push(value.clone());
                }
            } else if comparable.starts_with(&format!("{flag}="))
                || comparable.starts_with(&format!("{flag}:"))
            {
                result.push(argument[flag.len() + 1..].to_owned());
            } else if flag.len() == 2 && comparable.starts_with(flag) && comparable.len() > 2 {
                result.push(argument[2..].to_owned());
            }
        }
    }
    result
}

fn positionals(args: &[String], valued_flags: &[&str]) -> Vec<String> {
    let mut result = Vec::new();
    let mut skip = false;
    let mut after_options = false;
    for argument in args {
        if skip {
            skip = false;
            continue;
        }
        if argument == "--" {
            after_options = true;
            continue;
        }
        if !after_options && argument.starts_with('-') {
            skip = valued_flags.iter().any(|flag| {
                if flag.len() == 2 {
                    argument == flag
                } else {
                    argument.eq_ignore_ascii_case(flag)
                }
            });
            continue;
        }
        result.push(argument.clone());
    }
    result
}
