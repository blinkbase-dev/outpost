use clap::Parser;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {}

fn main() {
	Args::parse();
}
