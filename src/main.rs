use indexmap::IndexMap;
use rustyline::history::FileHistory;
use std::fs::File;
#[allow(unused_imports)]
use std::io::{self, Write};
use std::io::{BufRead, BufReader, BufWriter};
use std::os::unix::process::ExitStatusExt;
use std::usize;
use std::{
    collections::HashMap,
    env,
    fs::{self, OpenOptions},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{LazyLock, Mutex},
};

use rustyline::{
    CompletionType, Config, Helper, Highlighter, Hinter, Validator,
    completion::{Completer, Pair},
};

struct BackgroundProcess {
    child: Child,
    command: String,
}

enum RedirectTarget<'a> {
    Stdout { filename: &'a str, append: bool },
    Stderr { filename: &'a str, append: bool },
}

struct ParsedCommand<'a> {
    args: Vec<&'a str>,
    redirect: Option<RedirectTarget<'a>>,
    background: bool,
}

static COMPLETE_MAP: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static JOBS_MAP: LazyLock<Mutex<IndexMap<usize, BackgroundProcess>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));
static HISTORY: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static HISTORY_APPEND_COUNTER: LazyLock<Mutex<usize>> = LazyLock::new(|| Mutex::new(0));
static HISTFILE_APPEND_COUNTER: Mutex<usize> = Mutex::new(0);
static DECLARE_MAP: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Helper, Hinter, Highlighter, Validator)]
struct ShellCompleter;

impl Completer for ShellCompleter {
    type Candidate = Pair;
    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        let arr: Vec<&str> = line.split_whitespace().collect();
        let mut args: Vec<&str> = Vec::new();
        let completion_script = arr
            .first()
            .and_then(|command| COMPLETE_MAP.lock().unwrap().get(*command).cloned());
        if let Some(completion_script) = completion_script {
            unsafe {
                env::set_var("COMP_LINE", line);
                env::set_var("COMP_POINT", pos.to_string());
            }
            let has_trailing_space = line.chars().last().is_some_and(char::is_whitespace);
            let current_word = if has_trailing_space {
                ""
            } else {
                arr.last().copied().unwrap_or("")
            };
            let previous_word = if has_trailing_space {
                arr.last().copied().unwrap_or("")
            } else if arr.len() >= 2 {
                arr[arr.len() - 2]
            } else {
                ""
            };
            args.extend([arr[0], current_word, previous_word]);
            let script_output = execute_script(&completion_script, &args);

            if script_output.is_empty() {
                return Ok((pos, Vec::new()));
            }

            let completion_start = pos - current_word.len();
            let mut pairs = Vec::new();
            for line in script_output.lines() {
                let replacement = if line == current_word {
                    line.to_string()
                } else {
                    format!("{} ", line)
                };
                pairs.push(Pair {
                    display: line.to_string(),
                    replacement,
                });
            }

            return Ok((completion_start, pairs));
        }
        let is_cmd_completion = !line.ends_with(" ") && arr.len() < 2;
        let mut matches = if is_cmd_completion {
            command_completion(line)
        } else {
            file_completion(&arr)
        };
        matches.sort_by(|a, b| a.display.cmp(&b.display));
        Ok((0, matches))
    }
}

fn execute_script(path: &str, args: &[&str]) -> String {
    let output = Command::new(path).args(args).output().unwrap();

    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn file_completion(line: &[&str]) -> Vec<Pair> {
    let search_prefix;
    let input = if line.len() < 2 {
        search_prefix = line[0].to_owned();
        ""
    } else {
        search_prefix = line[0..(line.len() - 1)].join(" ").to_owned();
        line[line.len() - 1]
    };
    let (dir, file_suff) = match input.rsplit_once('/') {
        Some((dir, file_suff)) => (dir, file_suff),
        None => (".", input),
    };

    let path_pref: &str = if dir == "." { "" } else { &format!("{dir}/") };

    let mut matches: Vec<Pair> = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.starts_with(file_suff) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.is_file() {
            matches.push(Pair {
                display: format!("{} ", name),
                replacement: format!("{} {}{} ", search_prefix, path_pref, name),
            })
        } else if metadata.is_dir() {
            matches.push(Pair {
                display: format!("{}/ ", name),
                replacement: format!("{} {}{}/", search_prefix, path_pref, name),
            })
        }
    }

    matches
}

fn command_completion(line: &str) -> Vec<Pair> {
    let supported_commands = ["echo ", "exit "];
    let mut matches: Vec<Pair> = supported_commands
        .iter()
        .filter(|command| command.starts_with(line))
        .map(|command| Pair {
            display: command.to_string(),
            replacement: command.to_string(),
        })
        .collect();
    let path_var = env::var("PATH").unwrap_or_default();
    for dir in path_var.split(":") {
        let path = Path::new(&dir);
        if !path.exists() || !path.is_dir() {
            continue;
        }

        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !name.starts_with(line) {
                continue;
            }
            if entry.path().is_dir() {
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                matches.push(Pair {
                    display: format!("{} ", name),
                    replacement: format!("{} ", name),
                })
            }
        }
    }
    matches
}

fn main() {
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .build();
    let mut rl = rustyline::Editor::with_config(config).unwrap();
    rl.set_helper(Some(ShellCompleter));
    if let Err(error) = handle_startup(&mut rl) {
        println!("Error: {error}");
    }
    loop {
        handle_jobs(true);
        let mut command = rl.readline("$ ").unwrap();
        command = command.trim_end().to_string();
        if command.is_empty() {
            continue;
        }
        HISTORY.lock().unwrap().push(command.clone());
        if let Err(err) = rl.add_history_entry(command.clone()) {
            eprintln!("Error: {err}");
        }
        if command.contains('|') {
            if let Err(err) = execute_pipeline_line(&command) {
                eprintln!("Error: {err}");
            }
            continue;
        }
        let mut parse = command.splitn(2, char::is_whitespace);
        let cmd = parse.next().unwrap();
        let args = parse.next().unwrap_or("").trim_start();
        match cmd {
            "exit" => {
                handle_exit(&mut rl);
                break;
            }
            "echo" => builtin_redirect(cmd, args),
            "pwd" => println!("{}", env::current_dir().unwrap().display()),
            "cd" => change_directory(args),
            "complete" => completion_command(cmd, args),
            "jobs" => handle_jobs(false),
            "history" => {
                if let Err(err) = handle_history(args) {
                    eprintln!("Error displaying history: {err}");
                }
            }
            "declare" => {
                if let Err(err) = handle_declare(args) {
                    eprintln!("Error in declare: {err}");
                }
            }
            "type" => match args {
                "type" | "echo" | "exit" | "pwd" | "cd" | "complete" | "jobs" | "history"
                | "declare" => {
                    println!("{} is a shell builtin", args)
                }
                _ => match check_executable(args) {
                    Some(x) => println!("{} is {}", args, x),
                    None => println!("{}: not found", args),
                },
            },
            _ => match check_executable(cmd) {
                Some(_) => {
                    if let Err(err) = execute_program(cmd, args) {
                        eprint!("Error: {err}");
                    }
                }
                None => println!("{}: command not found", cmd),
            },
        }
    }
    fn handle_declare(args: &str) -> io::Result<()> {
        let mut parts = args.split_whitespace();
        match parts.next() {
            Some("-p") => {
                let variable = parts.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "expected a variable name")
                })?;
                if parts.next().is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Unexpect string after variable",
                    ));
                }
                match DECLARE_MAP.lock().unwrap().get(variable) {
                    Some(value) => println!("declare -- {variable}=\"{value}\""),
                    None => eprintln!("declare: {variable}: not found"),
                }
                Ok(())
            }
            Some(var_txt) => {
                let (first, second) = var_txt.split_once("=").ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "expected a variables")
                })?;
                if !is_valid_variable_name(first) {
                    eprintln!("declare: `{var_txt}': not a valid identifier");
                    return Ok(());
                }
                DECLARE_MAP
                    .lock()
                    .unwrap()
                    .insert(first.to_string(), second.to_string());
                Ok(())
            }
            _ => Ok(()),
        }
    }
    fn is_valid_variable_name(name: &str) -> bool {
        let mut chars = name.chars();
        matches!(chars.next(), Some('_' | 'a'..='z' | 'A'..='Z'))
            && chars.all(|c| matches!(c, '_' | 'a'..='z' | 'A'..='Z' | '0'..='9' ))
    }
    fn handle_startup(rl: &mut rustyline::Editor<ShellCompleter, FileHistory>) -> io::Result<()> {
        if let Some(histfile) = env::var("HISTFILE").ok() {
            read_history(&histfile);
            let history = {
                let history = HISTORY.lock().unwrap();
                history.clone()
            };
            for hist in history.iter() {
                rl.add_history_entry(hist);
            }
            *HISTFILE_APPEND_COUNTER.lock().unwrap() = history.len();
        }

        Ok(())
    }
    fn handle_exit(rl: &mut rustyline::Editor<ShellCompleter, FileHistory>) {
        if let Ok(histfile) = env::var("HISTFILE") {
            if let Err(err) = write_history(
                &histfile,
                false,
                true,
                *HISTFILE_APPEND_COUNTER.lock().unwrap(),
            ) {
                eprintln!("Error: {err}");
            }
        }
    }
    fn read_history(path: &str) -> io::Result<()> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut history = HISTORY.lock().unwrap();
        for line in reader.lines() {
            let line = line?;
            history.push(line);
        }
        Ok(())
    }
    fn write_history(path: &str, truncate: bool, append: bool, skip: usize) -> io::Result<()> {
        let file = OpenOptions::new()
            .create(true)
            .append(append)
            .truncate(truncate)
            .write(true)
            .open(path)?;
        let mut writer = BufWriter::new(file);
        let history = {
            let history = HISTORY.lock().unwrap();
            history.clone()
        };
        for line in history.iter().skip(skip) {
            writeln!(writer, "{line}")?;
        }
        writer.flush()?;

        Ok(())
    }
    fn handle_history(args: &str) -> io::Result<()> {
        let mut parts = args.split_whitespace();
        match parts.next() {
            Some("-r") => {
                let path = parts.next().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "expected file path to read from",
                    )
                })?;
                if parts.next().is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "unexpected extra arguments for history -r",
                    ));
                }
                read_history(path)
            }
            Some("-w") => {
                let path = parts.next().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "expected file path to write history to",
                    )
                })?;
                write_history(path, true, false, 0)
            }
            Some("-a") => {
                let path = parts.next().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "expected file path to write history to",
                    )
                })?;
                let mut counter = HISTORY_APPEND_COUNTER.lock().unwrap();
                let skip = *counter;
                *counter = HISTORY.lock().unwrap().len();
                write_history(path, false, true, skip)
            }
            Some(limit) => {
                if parts.next().is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "unexpected history arguments",
                    ));
                }
                let limit = limit.parse::<usize>().map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("invalid history limit: {err}"),
                    )
                })?;
                print_history(limit)
            }
            None => print_history(usize::MAX),
        }
    }
    fn print_history(limit: usize) -> io::Result<()> {
        let history = HISTORY.lock().unwrap();

        let skip = history.len().saturating_sub(limit);
        let stdout = io::stdout();
        let mut out = stdout.lock();

        for (i, cmd) in history.iter().enumerate().skip(skip) {
            writeln!(out, "{} {}", i + 1, cmd)?;
        }
        Ok(())
    }

    fn builtin_pipeline_output(cmd: &str, args: &[&str]) -> io::Result<Option<Vec<u8>>> {
        let mut output = Vec::new();
        match cmd {
            "echo" => {
                writeln!(&mut output, "{}", args.join(" "))?;
            }
            "pwd" => {
                writeln!(&mut output, "{}", env::current_dir()?.display())?;
            }
            "type" => match args[0] {
                "type" | "echo" | "exit" | "pwd" | "cd" | "complete" | "jobs" => {
                    writeln!(&mut output, "{} is a shell builtin", args[0])?;
                }
                _ => match check_executable(args[0]) {
                    Some(x) => {
                        writeln!(&mut output, "{} is {}", args[0], x)?;
                    }
                    None => {
                        writeln!(&mut output, "{}: not found", args[0])?;
                    }
                },
            },
            _ => return Ok(None),
        }
        Ok(Some(output))
    }

    fn execute_pipeline_line(line: &str) -> io::Result<()> {
        let stages: Vec<&str> = line.split('|').map(str::trim).collect();

        if stages.len() < 2 || stages.iter().any(|stage| stage.is_empty()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid pipeline",
            ));
        }

        // Parse the first command.
        let mut first_parts = stages[0].split_whitespace();
        let first_name = first_parts
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing command"))?;
        let first_args: Vec<&str> = first_parts.collect();

        // Built-ins are supported as the first pipeline stage.
        // Their output is buffered and fed to the next process.
        let builtin_output = builtin_pipeline_output(first_name, &first_args)?;

        let builtin_first = builtin_output.is_some();
        let start_index = if builtin_first { 1 } else { 0 };

        let mut children: Vec<Child> = Vec::new();
        let mut previous_stdout = None;
        let mut builtin_stdin = None;

        // Spawn every external stage before writing buffered built-in output.
        // This allows downstream processes to consume data while it is written.
        for (index, stage) in stages.iter().enumerate().skip(start_index) {
            let mut parts = stage.split_whitespace();

            let name = parts.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "Missing command in pipeline")
            })?;

            let args: Vec<&str> = parts.map(|arg| arg.trim_matches('"')).collect();

            // For this version, built-ins are supported only as the first stage.
            let is_last = index == stages.len() - 1;

            if ["echo", "pwd", "type"].contains(&name) && is_last {
                let output = builtin_pipeline_output(name, &args)?
                    .ok_or_else(|| io::Error::other("Failed to execute pipeline builtin"))?;

                // This builtin doesn't consume upstream stdin.
                // Close the pipe reader so the upstream process won't block forever.
                drop(previous_stdout.take());

                io::stdout().write_all(&output)?;
                continue;
            }

            if ["echo", "type", "cd"].contains(&name) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Built-in '{name}' is only supported as the first pipeline command"),
                ));
            }

            let path = check_executable(name).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{name}: command not found"),
                )
            })?;

            let is_last = index == stages.len() - 1;

            let mut command = Command::new(path);
            command.args(&args);

            if builtin_first && index == start_index {
                // Feed the built-in's buffered output into the first process.
                command.stdin(Stdio::piped());
            } else if let Some(stdout) = previous_stdout.take() {
                // Connect the preceding process to this process.
                command.stdin(Stdio::from(stdout));
            } else if index != 0 {
                return Err(io::Error::other("Missing pipe from previous command"));
            }

            if !is_last {
                command.stdout(Stdio::piped());
            }
            // The final command inherits stdout, so output reaches the terminal.

            let mut child = command.spawn()?;

            if builtin_first && index == start_index {
                builtin_stdin = child.stdin.take();
            }

            if !is_last {
                previous_stdout = Some(
                    child
                        .stdout
                        .take()
                        .ok_or_else(|| io::Error::other("Failed to obtain pipeline stdout"))?,
                );
            }

            children.push(child);
        }

        // Write the built-in's output after downstream processes have started.
        if let Some(output) = builtin_output {
            if let Some(mut stdin) = builtin_stdin {
                if let Err(err) = stdin.write_all(&output) {
                    if err.kind() != io::ErrorKind::BrokenPipe {
                        return Err(err);
                    }
                }
                // stdin is dropped here, signalling EOF to the receiving command.
            }
        }

        // Reap every child process.
        let mut last_status = None;

        for mut child in children {
            let status = child.wait()?;
            last_status = Some(status);
        }

        if let Some(status) = last_status {
            if !status.success() && status.signal() != Some(13) {
                eprintln!("Pipeline command exited with {status}");
            }
        }

        Ok(())
    }

    fn handle_jobs(auto: bool) {
        let no_of_elements = JOBS_MAP.lock().unwrap().len();
        let mut done_jobs = Vec::new();
        for (counter, (id, bg_process)) in (1..).zip(JOBS_MAP.lock().unwrap().iter_mut()) {
            let symbol = if counter == no_of_elements {
                "+"
            } else if counter + 1 == no_of_elements {
                "-"
            } else {
                " "
            };
            let mut completed = false;
            let status = match bg_process.child.try_wait() {
                Ok(Some(_)) => {
                    done_jobs.push(*id);
                    completed = true;
                    "Done"
                }
                Ok(None) => "Running",
                Err(_) => "Running",
            };
            if !completed && auto {
                continue;
            }
            println!(
                "[{}]{}  {}{}{}",
                id,
                symbol,
                status,
                " ".repeat(24 - status.len()),
                bg_process.command
            );
        }
        for id in done_jobs {
            JOBS_MAP.lock().unwrap().swap_remove(&id);
        }
    }

    fn completion_command(_cmd: &str, args: &str) {
        let params: Vec<&str> = args.split_whitespace().collect();

        match params[0] {
            "-C" => COMPLETE_MAP
                .lock()
                .unwrap()
                .insert(params[2].to_string(), params[1].to_string()),
            "-p" => {
                match COMPLETE_MAP.lock().unwrap().get(params[1]) {
                    Some(val) => println!("complete -C '{val}' {}", params[1]),
                    None => println!("complete: {}: no completion specification", params[1]),
                };
                None
            }
            "-r" => {
                COMPLETE_MAP.lock().unwrap().remove(params[1]);
                None
            }
            _ => None,
        };
    }

    fn change_directory(args: &str) {
        let path: Vec<&str> = args.split(" ").collect();
        let dir_path = if path[0] == "~" {
            env::var("HOME").expect("Requires Home path")
        } else {
            path[0].to_string()
        };
        let dir = Path::new(&dir_path);
        if dir.is_dir() {
            env::set_current_dir(dir).unwrap();
        } else {
            println!("cd: {}: No such file or directory", path[0]);
        }
    }

    fn check_executable(args: &str) -> Option<String> {
        let path = env::var("PATH").unwrap();
        let paths: Vec<&str> = path.split(":").collect();
        for dir in paths {
            let file = Path::new(dir).join(args);
            if file.is_file() {
                let metadata = fs::metadata(&file).unwrap();
                if metadata.permissions().mode() & 0o111 != 0 {
                    return Some(file.to_str().unwrap().to_owned());
                }
            }
        }
        None
    }

    fn execute_pipeline(first_cmd_path: &str, args: &str) -> io::Result<()> {
        let (first_args, second_cmd) = args
            .split_once("|")
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Expected a pipe"))?;
        let mut second_parts = second_cmd.split_whitespace();
        let second_cmd_path = second_parts.next().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Missing command after pipe")
        })?;

        let second_cmd_args: Vec<&str> = second_parts.collect();
        let mut first = Command::new(first_cmd_path)
            .args(first_args.split_whitespace())
            .stdout(Stdio::piped())
            .spawn()?;
        let pipe = first
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("Failed to obtain pipeline stdout"))?;

        let mut second = Command::new(second_cmd_path)
            .args(second_cmd_args)
            .stdin(Stdio::from(pipe))
            .spawn()?;

        let second_status = second.wait()?;
        let _first_status = first.wait()?;

        // Return an error if the second command could not succeed.
        if !second_status.success() {
            eprintln!("Pipeline's second command exited with {second_status}");
        }
        Ok(())
    }

    fn execute_program(cmd_path: &str, args: &str) -> io::Result<()> {
        if args.contains("|") {
            return execute_pipeline(cmd_path, args);
        }
        let parsed =
            parse_args(args).map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        let ParsedCommand {
            args: cmd_args,
            redirect,
            background,
        } = parsed;
        let mut cmd = Command::new(cmd_path);
        cmd.args(&cmd_args);

        apply_redirection(&mut cmd, redirect)?;

        if background {
            let child = cmd.spawn()?;
            let pid = child.id();
            let command = format!("{} {}", cmd_path, cmd_args.join(" "));
            let mut jobs = JOBS_MAP.lock().unwrap();
            let id = jobs.len() + 1;

            println!("[{}] {}", id, pid);
            jobs.insert(id, BackgroundProcess { child, command });
        } else {
            cmd.status()?;
        }

        Ok(())
    }

    fn parse_args(args: &str) -> Result<ParsedCommand<'_>, String> {
        let mut tokens: Vec<&str> = args.split_whitespace().collect();

        let background = tokens.last() == Some(&"&");

        if background {
            tokens.pop();
        }

        let mut cmd_args = Vec::new();
        let mut redirect = None;
        let mut iter = tokens.into_iter();

        while let Some(arg) = iter.next() {
            let redirect_target = match arg {
                ">" | "1>" => Some((false, false)),
                ">>" | "1>>" => Some((false, true)),
                "2>" => Some((true, false)),
                "2>>" => Some((true, true)),
                "|" => {
                    return Err("Pipelines are not supported by this parser yet".into());
                }
                _ => None,
            };

            if let Some((is_stderr, append)) = redirect_target {
                if redirect.is_some() {
                    return Err("Multiple redirections are not supported yet".into());
                }

                let filename = iter
                    .next()
                    .ok_or_else(|| format!("Missing filename after '{}'", arg))?;

                redirect = Some(if is_stderr {
                    RedirectTarget::Stderr { filename, append }
                } else {
                    RedirectTarget::Stdout { filename, append }
                });

                continue;
            }

            cmd_args.push(arg);
        }

        Ok(ParsedCommand {
            args: cmd_args,
            redirect,
            background,
        })
    }

    fn apply_redirection(
        cmd: &mut Command,
        redirect: Option<RedirectTarget<'_>>,
    ) -> io::Result<()> {
        let Some(redirect) = redirect else {
            return Ok(());
        };
        let (filename, append, is_stderr) = match redirect {
            RedirectTarget::Stderr { filename, append } => (filename, append, true),
            RedirectTarget::Stdout { filename, append } => (filename, append, false),
        };
        let mut options = OpenOptions::new();
        options.write(true).create(true);

        if append {
            options.append(true);
        } else {
            options.truncate(true);
        }

        let file = options.open(filename)?;
        if is_stderr {
            cmd.stderr(Stdio::from(file));
        } else {
            cmd.stdout(Stdio::from(file));
        }
        Ok(())
    }

    fn builtin_redirect(_cmd: &str, args: &str) {
        let mut output: Box<dyn Write> = Box::new(io::stdout());
        let (output_file, cmd_args, is_err_redirect, append) = rediret_filename(args);
        if let Some(filename) = output_file {
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(!append)
                .append(append)
                .open(filename)
                .expect("Can't create file");
            if !is_err_redirect {
                output = Box::new(file);
            }
        }
        writeln!(output, "{}", cmd_args.join(" ")).unwrap();
    }

    fn rediret_filename(args: &str) -> (Option<&str>, Vec<&str>, bool, bool) {
        let mut iter = args.split_whitespace();
        let mut output_file = None;
        let mut cmd_args = Vec::new();
        let mut is_err_redirect = false;
        let mut append = false;
        while let Some(arg) = iter.next() {
            if arg == ">" || arg == "1>" || arg == ">>" || arg == "1>>" {
                if arg == ">>" || arg == "1>>" {
                    append = true;
                }
                output_file = iter.next();
                break;
            }
            if arg == "2>" || arg == "2>>" {
                if arg == "2>>" {
                    append = true;
                }
                is_err_redirect = true;
                output_file = iter.next();
                break;
            }
            cmd_args.push(arg);
        }
        (output_file, cmd_args, is_err_redirect, append)
    }
}

#[test]
fn test1() {
    let text = "This   is   My";
    let vec: Vec<&str> = text.split_whitespace().collect();
    println!("{:?}", vec);
}
