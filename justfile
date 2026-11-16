set windows-shell := ["cmd.exe", "/c"]

build-trampoline *args:
	cargo rustc {{args}} -p outpost --features trampoline --bin op_shim -- -C panic=abort
