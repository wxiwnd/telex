fn main() {
    if let Err(err) = telex::cli::run() {
        eprintln!("tlx: {err:#}");
        std::process::exit(1);
    }
}
