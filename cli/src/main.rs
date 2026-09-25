fn main() {
    if let Err(error) = tessera_cli::run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
