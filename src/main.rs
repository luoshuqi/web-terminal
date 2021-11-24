use std::error::Error;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use log::{debug, error, info};
use structopt::StructOpt;
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Builder;
use ws::request::Request;
use ws::response::{Response, Status, NOT_FOUND, OK};
use ws::websocket::WebSocket;

macro_rules! c {
    ($($args:tt)+) => {
        match libc::$($args)* {
            -1 => return Err(io::Error::last_os_error()),
            ret => ret,
        }
    };
}

mod pty;
mod terminal;

#[derive(StructOpt)]
struct Opt {
    /// 要绑定的地址，格式 ip:port
    #[structopt(short, long)]
    bind: SocketAddr,

    /// 登录用户名，系统上不需要存在此用户
    #[structopt(short, long)]
    user: String,

    /// 登录密码
    #[structopt(short, long)]
    password: String,
}

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let opt: Arc<Opt> = Arc::new(Opt::from_args());

    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let listener = TcpListener::bind(opt.bind).await?;
            info!("server started at {}", listener.local_addr()?);
            loop {
                let (stream, addr) = listener.accept().await?;
                debug!("new connection from {}", addr);
                let opt = Arc::clone(&opt);
                tokio::spawn(async move {
                    match handle_client(stream, opt).await {
                        Ok(()) => {}
                        Err(err) => error!("{} {:?}", addr, err),
                    }
                });
            }
        })
}

async fn handle_client(mut stream: TcpStream, opt: Arc<Opt>) -> Result<(), Box<dyn Error>> {
    let mut buf = vec![0; 2048];
    let req = Request::new(&mut stream, &mut buf, Duration::from_secs(60)).await?;

    if !login(&req, &opt) {
        return basic_auth(&mut stream).await.map_err(Into::into);
    }

    match req.uri().split('?').next().unwrap() {
        "/" => {
            let mut response = Response::bytes(OK, include_bytes!("index.html"));
            response.add_header("content-type", "text/html;charset=UTF-8");
            response.write(&mut stream).await?;
            Ok(())
        }
        "/ws" => match WebSocket::upgrade(&req, stream).await? {
            Some(ws) => terminal::start(ws, "/bin/bash").await,
            None => Ok(()),
        },
        _ => {
            Response::status(NOT_FOUND).write(&mut stream).await?;
            Ok(())
        }
    }
}

async fn basic_auth(stream: &mut TcpStream) -> io::Result<()> {
    const UNAUTHORIZED: Status = Status(401, "Unauthorized");
    let mut response = Response::status(UNAUTHORIZED);
    response.add_header("WWW-Authenticate", "Basic realm=\"web terminal\"");
    response.write(stream).await?;
    return Ok(());
}

fn login(req: &Request, opt: &Arc<Opt>) -> bool {
    match get_auth(req) {
        Some((user, password)) if user == opt.user && password == opt.password => true,
        _ => false,
    }
}

fn get_auth(req: &Request) -> Option<(String, String)> {
    let auth = req.header("Authorization")?;
    if !auth.starts_with("Basic ") {
        return None;
    }

    let auth = String::from_utf8(base64::decode(&auth[6..]).ok()?).ok()?;
    let mut s = auth.split(':');
    Some((s.next()?.to_string(), s.next()?.to_string()))
}
