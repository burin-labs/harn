use super::*;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

fn parent(port: u16, finish: &str) -> String {
    let descendant = format!(
        "import socket;s=socket.create_connection(('127.0.0.1',{port}));s.sendall(b'ready');print('ready',flush=True);s.recv(1);s.sendall(b'alive')"
    );
    format!(
        "import os,subprocess,sys;child=subprocess.Popen([sys.executable,'-c',{descendant:?}],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL);assert child.stdout.readline()==b'ready\\n';print('synthetic-cli-token',flush=True);{finish}"
    )
}

async fn ready(listener: &TcpListener) -> TcpStream {
    let (mut stream, _) = tokio::time::timeout(DEADLINE, listener.accept())
        .await
        .expect("descendant must connect")
        .expect("accept descendant");
    let mut bytes = [0; 5];
    tokio::time::timeout(DEADLINE, stream.read_exact(&mut bytes))
        .await
        .expect("descendant must report readiness")
        .expect("read descendant readiness");
    assert_eq!(&bytes, b"ready");
    stream
}

#[tokio::test]
async fn private_capture_reaps_descendants_on_completion_and_timeout() {
    // Calibrate the same child: without capture cleanup it stays connected
    // until explicitly released, then reports that it survived its parent.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut control = tokio::process::Command::new("python3");
    control.args(["-c", &parent(listener.local_addr().unwrap().port(), "pass")]);
    let (status, mut stream) = tokio::join!(control.status(), ready(&listener));
    assert!(status.unwrap().success());
    assert_eq!(
        stream.try_read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "unmanaged child must still hold its connection"
    );
    stream.write_all(b"x").await.unwrap();
    let mut survived = Vec::new();
    tokio::time::timeout(DEADLINE, stream.read_to_end(&mut survived))
        .await
        .expect("released control must finish")
        .unwrap();
    assert_eq!(survived, b"alive");

    for (finish, refuses) in [
        ("pass", false),
        ("sys.stdout.close();os.close(1);child.wait()", false),
        ("child.wait()", true),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut command = tokio::process::Command::new("python3");
        command.args(["-c", &parent(listener.local_addr().unwrap().port(), finish)]);
        let (result, mut stream) = tokio::join!(capture(command, DEADLINE), ready(&listener));
        assert_eq!(result.is_err(), refuses, "parent: {finish}");
        let mut survivor = Vec::new();
        tokio::time::timeout(DEADLINE, stream.read_to_end(&mut survivor))
            .await
            .expect("capture must close the reached descendant's connection")
            .unwrap();
        assert!(survivor.is_empty(), "captured child must not survive");
    }
}
