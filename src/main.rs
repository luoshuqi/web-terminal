use std::error::Error;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use log::{debug, error, info};
use nix::unistd::{getuid, User};
use pam_client::conv_mock::Conversation;
use pam_client::{Context, Flag};
use structopt::StructOpt;
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Builder;
use ws::request::Request;
use ws::response::{Response, Status, NOT_FOUND, OK};
use ws::websocket::WebSocket;

mod pty;
mod terminal;

#[derive(StructOpt)]
struct Opt {
    /// 要绑定的地址，格式 ip:port
    #[structopt(short, long)]
    bind: SocketAddr,
}

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    if !getuid().is_root() {
        info!("当前以非 root 权限执行，只有执行本程序的用户可以登录");
    }

    let opt: Opt = Opt::from_args();

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
                tokio::spawn(async move {
                    match handle_client(stream).await {
                        Ok(()) => {}
                        Err(err) => error!("{} {:?}", addr, err),
                    }
                });
            }
        })
}

async fn handle_client(mut stream: TcpStream) -> Result<(), Box<dyn Error>> {
    let mut buf = vec![0; 2048];
    let req = Request::new(&mut stream, &mut buf, Duration::from_secs(60)).await?;

    let ctx = match get_auth(&req) {
        Some((user, password)) => match pam_auth(user, password) {
            Ok(ctx) => ctx,
            Err(err) => {
                error!("pam_auth: {:?}", err);
                return Ok(basic_auth(&mut stream).await?);
            }
        },
        _ => return Ok(basic_auth(&mut stream).await?),
    };
    let user = User::from_name(&ctx.user()?)?.unwrap();
    let shell = &user.shell;
    if shell.ends_with("false") || shell.ends_with("nologin") {
        return Ok(basic_auth(&mut stream).await?);
    }

    match req.uri().split('?').next().unwrap() {
        "/" => {
            let mut response = Response::bytes(OK, include_bytes!("index.html"));
            response.add_header("content-type", "text/html;charset=UTF-8");
            response.write(&mut stream).await?;
            Ok(())
        }
        "/ws" => match WebSocket::upgrade(&req, stream).await? {
            Some(ws) => terminal::start(ws, user, ctx).await,
            None => Ok(()),
        },
        _ => {
            Response::status(NOT_FOUND).write(&mut stream).await?;
            Ok(())
        }
    }
}

fn pam_auth(user: String, password: String) -> pam_client::Result<Context<Conversation>> {
    let conv = Conversation::with_credentials(user, password);
    let mut ctx = Context::new("web-terminal", None, conv)?;
    ctx.authenticate(Flag::NONE)?;
    ctx.acct_mgmt(Flag::NONE)?;
    Ok(ctx)
}

async fn basic_auth(stream: &mut TcpStream) -> io::Result<()> {
    const UNAUTHORIZED: Status = Status(401, "Unauthorized");
    let mut response = Response::status(UNAUTHORIZED);
    response.add_header("WWW-Authenticate", "Basic realm=\"web terminal\"");
    response.write(stream).await?;
    return Ok(());
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
