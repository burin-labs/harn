pub struct Server {
    pub host: String,
    pub port: u16,
}

impl Server {
    pub fn new(host: String, port: u16) -> Self {
        Self { host, port }
    }
}

pub fn start_server(host: String, port: u16) -> Server {
    let server = Server::new(host, port);
    server
}

pub fn stop_server(_srv: &Server) {
    drop(_srv);
}
