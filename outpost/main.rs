mod backend;
mod config;
mod exec;
mod install;
mod list;
mod shim;
mod tools;
mod ui;

use clap::{Parser, Subcommand};

fn main() {
	let result = match Args::parse().command {
		Command::Install => install::install().map(|()| 0),
		Command::List => list::list().map(|()| 0),
		Command::Exec(args) => exec::exec(args),
		Command::Shim(args) => shim::shim(args).map(|()| 0),
	};
	match result {
		Ok(code) => std::process::exit(code),
		Err(error) => {
			eprintln!("{error}");
			std::process::exit(1);
		}
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
	Exec(exec::ExecArgs),
	Shim(shim::ShimArgs),
}
