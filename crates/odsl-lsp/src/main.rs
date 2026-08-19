fn main() {
    if let Err(e) = odsl_lsp::run_stdio() {
        eprintln!("odsl-lsp fatal: {e}");
        std::process::exit(1);
    }
}
