#[allow(unused_imports)]
use std::io::{self, Write};
use std::{
    collections::HashMap,
    env,
    fmt::format,
    fs::{self, File, FileType, OpenOptions},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    sync::{LazyLock, Mutex},
};

use rustyline::{
    CompletionType, Config, Context, Editor, Helper, Highlighter, Hinter, Validator,
    completion::{Completer, Pair},
    error::ReadlineError,
};

static COMPLETE_MAP: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Helper, Hinter, Highlighter, Validator)]
struct ShellCompleter;

impl Completer for ShellCompleter {
    type Candidate = Pair;
    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        let arr: Vec<&str> = line.split_whitespace().collect();
        let mut args: Vec<&str> = Vec::new();
        let mut pref: Vec<&str> = Vec::new();
        let completion_script = arr
            .first()
            .and_then(|command| COMPLETE_MAP.lock().unwrap().get(*command).cloned());
        if let Some(completion_script) = completion_script {
            unsafe {
                env::set_var("COMP_LINE", line);
                env::set_var("COMP_POINT", &pos.to_string());
            }
            if arr.len() == 1 {
                args.push("");
                pref.push(arr[0]);
            } else if arr.len() == 2 {
                args.push(arr[0]);
                args.push(arr[1]);
                pref.push(arr[0]);
                pref.push(arr[1]);
            } else {
                args.push(arr[0]);
                args.push(arr[arr.len() - 1]);
                args.push(arr[arr.len() - 2]);
                pref.extend(&arr[0..arr.len() - 1]);
            };
            let script_output = execute_script(&completion_script, &args);

            if script_output.is_empty() {
                return Ok((pos, Vec::new()));
            }

            let replacement = if script_output == *arr.last().unwrap() {
                format!("{} {}", pref.join(" "), script_output)
            } else {
                format!("{} {} ", pref.join(" "), script_output)
            };

            return Ok((
                0,
                vec![Pair {
                    display: script_output,
                    replacement,
                }],
            ));
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
                display: format!("{} ", name.to_string()),
                replacement: format!("{} {}{} ", search_prefix, path_pref, name.to_string()),
            })
        } else if metadata.is_dir() {
            matches.push(Pair {
                display: format!("{}/ ", name.to_string()),
                replacement: format!("{} {}{}/", search_prefix, path_pref, name.to_string()),
            })
        }
    }

    matches
}

fn command_completion(line: &str) -> Vec<Pair> {
    let supported_commands = vec!["echo ", "exit "];
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

            if metadata.is_file() {
                if metadata.permissions().mode() & 0o111 != 0 {
                    matches.push(Pair {
                        display: format!("{} ", name.to_string()),
                        replacement: format!("{} ", name.to_string()),
                    })
                }
            }
        }
    }
    matches
}

fn main() {
    let BUILTIN_COMMANDS = vec!["type", "echo", "exit", "pwd", "cd"];
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .build();
    let mut rl = rustyline::Editor::with_config(config).unwrap();
    rl.set_helper(Some(ShellCompleter));
    loop {
        let mut command = rl.readline("$ ").unwrap();
        command = command.trim_end().to_string();
        if command == "" {
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
            "type" => match args {
                "type" | "echo" | "exit" | "pwd" | "cd" | "complete" => {
                    println!("{} is a shell builtin", args)
                }
                _ => match check_executable(args) {
                    Some(x) => println!("{} is {}", args, x),
                    None => println!("{}: not found", args),
                },
            },
            _ => match check_executable(cmd) {
                Some(path) => execute_program(cmd, args),
                None => println!("{}: command not found", cmd),
            },
        }
    }

    fn completion_command(cmd: &str, args: &str) {
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

    fn execute_program(cmd_path: &str, args: &str) {
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
        let mut cmd = Command::new(cmd_path);
        cmd.args(&cmd_args);
        if let Some(filename) = output_file {
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .append(append)
                .truncate(!append)
                .open(filename)
                .expect("Can't create file");
            if is_err_redirect {
                cmd.stderr(Stdio::from(file));
            } else {
                cmd.stdout(Stdio::from(file));
            }
        }
        cmd.status();
    }

    fn builtin_redirect(cmd: &str, args: &str) {
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
        write!(output, "{}\n", cmd_args.join(" ")).unwrap();
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
