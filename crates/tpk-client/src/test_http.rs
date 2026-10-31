use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

pub(crate) fn serve(response: Vec<u8>) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/pack?token=secret",
        listener.local_addr().unwrap()
    );
    let handle = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        // Oversized bodies may be refused before the server finishes writing.
        let _ = socket.write_all(&response);
        String::from_utf8(request).unwrap()
    });
    (url, handle)
}

pub(crate) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}
