use clap::Parser;
use oxo_worker::config::Config;

fn main() {
    let config = Config::parse();
    println!("oxo-worker {}", config.worker_name());
}
