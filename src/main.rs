#[allow(unused_imports)]
use std::io::{self, Write};

fn main() {
    loop {
        print!("$ ");
        io::stdout().flush().unwrap();
        let mut command = String::new();
        io::stdin().read_line(&mut command).unwrap();
        command = command.trim_end().to_string();
        let mut parse = command.splitn(2, char::is_whitespace);
        let cmd = parse.next().unwrap();
        let args = parse.next().unwrap_or("").trim_start();
        match cmd {
            "exit" => break,
            "echo" => println!("{args}"),
            "type" => match args {
                "type" | "echo" | "exit" => println!("{} is a shell builtin", args),
                _ => println!("{}: not found", args),
            },
            _ => println!("{}: command not found", cmd),
        }
    }
}
