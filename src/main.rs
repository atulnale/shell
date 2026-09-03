#[allow(unused_imports)]
use std::io::{self, Write};
use std::{env, path::Path};

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
                _ => check_executable(args),
            },
            _ => println!("{}: command not found", cmd),
        }
    }

    fn check_executable(args: &str) {
        let path = env::var("PATH").unwrap();
        let mut paths: Vec<&str> = path.split(":").collect();
        paths.sort();
        for dir in paths {
            let file = Path::new(dir).join(args);
            if file.is_file() {
                println!("{} is {}", args, file.to_str().unwrap());
                return;
            }
        }
        println!("{}: not found", args);
    }
}
