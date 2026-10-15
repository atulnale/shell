use indexmap::IndexMap;
#[allow(unused_imports)]
use std::io::{self, Write};
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
    loop {
        handle_jobs(true);
        let mut command = rl.readline("$ ").unwrap();
        command = command.trim_end().to_string();
        if command.is_empty() {
            continue;
        }
        let mut parse = command.splitn(2, char::is_whitespace);
        let cmd = parse.next().unwrap();
        let args = parse.next().unwrap_or("").trim_start();
        match cmd {
            "exit" => break,
            "echo" => builtin_redirect(cmd, args),
            "pwd" => println!("{}", env::current_dir().unwrap().display()),
            "cd" => change_directory(args),
            "complete" => completion_command(cmd, args),
            "jobs" => handle_jobs(false),
            "type" => match args {
                "type" | "echo" | "exit" | "pwd" | "cd" | "complete" | "jobs" => {
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
