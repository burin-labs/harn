mod config;
mod handler;
mod server;

use config::{config_url, load_config};
use handler::{handle_error, handle_request};
use server::{start_server, stop_server};

pub fn main() {
    let cfg = load_config();
    let url = config_url(&cfg);
    let srv = start_server(cfg.host.clone(), cfg.port);
    let body = handle_request("/health");
    let _ = handle_error("test");
    println!("{url} -> {body}");
    stop_server(&srv);
}
