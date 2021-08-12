use std::net::SocketAddr;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    signal::unix::{signal, SignalKind},
    sync::{broadcast, mpsc},
    time,
};

fn main() {
    tokio::runtime::Builder::new()
        .threaded_scheduler()
        .enable_all()
        .build()
        .unwrap()
        .block_on(start_server());
}

#[derive(Clone, Debug)]
struct ClientMessage(SocketAddr, String);

#[derive(Debug)]
enum ServerMessage {
    ClientError((SocketAddr, String)),
    Connected(SocketAddr),
    Disconnected(SocketAddr),
    ListenerError(String),
    Recevied((SocketAddr, String)),
    Signal(libc::c_int),
    Tick,
}

#[derive(Clone)]
struct ServerActor {
    server_sender: mpsc::Sender<ServerMessage>,
    client_sender: broadcast::Sender<ClientMessage>,
}

impl ServerActor {
    pub async fn send(
        &mut self,
        msg: ServerMessage,
    ) -> Result<(), mpsc::error::SendError<ServerMessage>> {
        self.server_sender.send(msg).await
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ClientMessage> {
        self.client_sender.subscribe()
    }
}

async fn start_server() {
    let (server_sender, mut server_receiver) = mpsc::channel(1);
    let (client_sender, _) = broadcast::channel(32);
    let server = ServerActor {
        server_sender,
        client_sender: client_sender.clone(),
    };

    start_signal_handler(server.clone());
    start_listener(server.clone());
    start_timer(server.clone());

    loop {
        match server_receiver.recv().await.expect("tasks are running") {
            ServerMessage::ClientError((addr, msg)) => {
                println!("Client error from {}: {}", addr, msg)
            }
            ServerMessage::Connected(addr) => println!("Connected {}", addr),
            ServerMessage::Disconnected(addr) => println!("Disconnected {}", addr),
            ServerMessage::ListenerError(msg) => println!("Listener error: {}", msg),
            ServerMessage::Recevied((addr, line)) => {
                println!("Received from {}: {}", addr, line.trim());
                client_sender.send(ClientMessage(addr, line)).unwrap();
            }
            ServerMessage::Signal(signal) => {
                println!("Signal: {}", signal);

                match signal {
                    libc::SIGINT | libc::SIGTERM => break,
                    libc::SIGHUP => (),
                    _ => panic!("unexpected signal"),
                }
            }
            ServerMessage::Tick => println!("Tick"),
        }
    }
}

fn start_signal_handler(mut server: ServerActor) {
    tokio::spawn(async move {
        let mut sigint = signal(SignalKind::interrupt()).unwrap();
        let mut sigterm = signal(SignalKind::terminate()).unwrap();
        let mut sighup = signal(SignalKind::hangup()).unwrap();

        loop {
            tokio::select! {
                _ = sigint.recv() => server.send(ServerMessage::Signal(libc::SIGINT)).await.unwrap(),
                _ = sigterm.recv() => server.send(ServerMessage::Signal(libc::SIGTERM)).await.unwrap(),
                _ = sighup.recv() => server.send(ServerMessage::Signal(libc::SIGHUP)).await.unwrap(),
            };
        }
    });
}

fn start_listener(mut server: ServerActor) {
    tokio::spawn(async move {
        let mut listener = TcpListener::bind("127.0.0.1:8080").await.unwrap();

        loop {
            match listener.accept().await {
                Ok((socket, addr)) => start_client(server.clone(), socket, addr),
                Err(error) => server
                    .send(ServerMessage::ListenerError(format!("{:?}", error)))
                    .await
                    .unwrap(),
            }
        }
    });
}

fn start_client(mut server: ServerActor, mut socket: TcpStream, addr: SocketAddr) {
    tokio::spawn(async move {
        server.send(ServerMessage::Connected(addr)).await.unwrap();

        let (reader, mut writer) = socket.split();
        let mut reader = BufReader::new(reader);
        let mut receiver = server.subscribe();

        loop {
            let mut line = String::new();

            tokio::select! {
                result = time::timeout(time::Duration::from_secs(30), reader.read_line(&mut line)) => match result {
                    Ok(result) => match result {
                        Ok(_) => {
                            line = line.trim().into();

                            if !line.is_empty() {
                                server
                                    .send(ServerMessage::Recevied((addr, line)))
                                    .await
                                    .unwrap();
                            }
                        }
                        Err(error) => {
                            server
                                .send(ServerMessage::ClientError((addr, format!("{:?}", error))))
                                .await
                                .unwrap();
                            break;
                        }
                    },
                    Err(error) => {
                        server
                            .send(ServerMessage::ClientError((addr, format!("{:?}", error))))
                            .await
                            .unwrap();
                        break;
                    }
                },
                result = receiver.recv() => {
                    if let Ok(ClientMessage(peer, line)) = result {
                        eprintln!("{} {} {}", addr, peer, line);
                        if let Err(error) = writer.write_all(format!("{:?}: {}\n", peer, line).as_bytes()).await {
                            server
                                .send(ServerMessage::ClientError((addr, format!("{:?}", error))))
                                .await
                                .unwrap();
                            break;
                        }
                    }
                },
            }
        }

        server
            .send(ServerMessage::Disconnected(addr))
            .await
            .unwrap();
    });
}

fn start_timer(mut server: ServerActor) {
    tokio::spawn(async move {
        let mut interval = time::interval(time::Duration::from_secs(1));

        loop {
            interval.tick().await;
            server.send(ServerMessage::Tick).await.unwrap();
        }
    });
}
