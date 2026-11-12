mod backend;
mod config;
mod install;
mod list;
mod tools;

use clap::{Parser, Subcommand};

fn main() {
	let result = match Args::parse().command {
		Command::Install => install::install(),
		Command::List => list::list(),
	};
	if let Err(error) = result {
		eprintln!("{error}");
		std::process::exit(1);
	}
}

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand)]
enum Command {
	Install,
	List,
}
