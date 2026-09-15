#[allow(unused_imports)]
use std::io::{self, Write};
use std::{
    env,
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
};

fn main() {
    let BUILTIN_COMMANDS = vec!["type", "echo", "exit", "pwd", "cd"];
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
            "echo" => builtin_redirect(cmd, args),
            "pwd" => println!("{}", env::current_dir().unwrap().display()),
            "cd" => change_directory(args),
            "type" => match args {
                "type" | "echo" | "exit" | "pwd" | "cd" => println!("{} is a shell builtin", args),
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
        while let Some(arg) = iter.next() {
            if arg == ">" || arg == "1>" {
                output_file = iter.next();
                break;
            }
            cmd_args.push(arg);
        }
        let mut cmd = Command::new(cmd_path);
        cmd.args(&cmd_args);
        if let Some(filename) = output_file {
            let file = File::create(filename).expect("Can't create file");
            cmd.stdout(Stdio::from(file));
        }

        cmd.status().unwrap();
    }

    fn builtin_redirect(cmd: &str, args: &str) {
        let mut output: Box<dyn Write> = Box::new(io::stdout());
        let (output_file, cmd_args) = rediret_filename(args);
        if let Some(filename) = output_file {
            output = Box::new(File::create(filename).expect("can't create file"));
        }
        write!(output, "{}\n", cmd_args.join(" ")).unwrap();
    }

    fn rediret_filename(args: &str) -> (Option<&str>, Vec<&str>) {
        let mut iter = args.split_whitespace();
        let mut output_file = None;
        let mut cmd_args = Vec::new();
        while let Some(arg) = iter.next() {
            if arg == ">" || arg == "1>" {
                output_file = iter.next();
                break;
            }
            cmd_args.push(arg);
        }
        (output_file, cmd_args)
    }
}

#[test]
fn test1() {
    let home = env::var("HOME").expect("Requires Home path");
    println!("{home}");
}
