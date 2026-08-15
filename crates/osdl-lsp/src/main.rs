fn main() {
    if let Err(e) = osdl_lsp::run_stdio() {
        eprintln!("osdl-lsp fatal: {e}");
        std::process::exit(1);
    }
}
