use clap::Parser;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {}

fn main() {
	Args::parse();
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_empty_args() {
		assert!(Args::try_parse_from(["op"]).is_ok());
	}
}
