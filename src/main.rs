#[allow(unused_imports)]
use std::io::{self, Write};
use std::{env, fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn main() {
    loop {
        print!("$ ");
        io::stdout().flush().unwrap();
        let mut command = String::new();
        io::stdin().read_line(&mut command).unwrap();
        command = command.trim_end().to_string();
        if command == "" {
            continue;
        }
        let mut parse = command.splitn(2, char::is_whitespace);
        let cmd = parse.next().unwrap();
        let args = parse.next().unwrap_or("").trim_start();
        match cmd {
            "exit" => break,
            "echo" => println!("{args}"),
            "type" => match args {
                "type" | "echo" | "exit" => println!("{} is a shell builtin", args),
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

    fn execute_program(cmd: &str, args: &str) {
        let vec_args: Vec<&str> = args.split_whitespace().collect();
        let status = Command::new(cmd).args(vec_args).status().unwrap();
    }
}
