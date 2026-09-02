#[allow(unused_imports)]
use std::io::{self, Write};

fn main() {
    loop {
        print!("$ ");
        io::stdout().flush().unwrap();
        let mut command = String::new();
        io::stdin().read_line(&mut command).unwrap();

        if command.trim_end() == String::from("exit") {
            break;
        }
        if command.starts_with("echo") {
            println!("{}", &command[5..]);
        } else {
            println!("{}: command not found", command.trim_end());
        }
    }
}
