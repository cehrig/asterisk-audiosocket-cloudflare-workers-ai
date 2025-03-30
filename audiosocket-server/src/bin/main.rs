use audio::Agent;
use std::env::args;
use std::net::TcpListener;
use std::sync::OnceLock;
use url::Url;

static URL: OnceLock<Url> = OnceLock::new();

fn main() {
    let port: u16 = args().nth(1).unwrap_or_default().parse().unwrap_or(3454);
    let url = URL.get_or_init(|| args().nth(2).unwrap().parse().unwrap());

    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).unwrap();

    for stream in listener.incoming() {
        Agent::run(stream.unwrap(), url);
    }
}
