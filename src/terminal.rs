use std::convert::Infallible;
use std::env::var;
use std::error::Error;
use std::ffi::{c_void, CString};
use std::io;
use std::os::raw::c_ushort;
use std::os::unix::io::{AsRawFd, RawFd};
use std::process::exit;
use std::thread::sleep;
use std::time::Duration;

use log::{debug, error, warn};
use nix::libc::{ioctl, winsize, TIOCSWINSZ};
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{chdir, execve, setgid, setuid, Pid, User};
use pam_client::conv_mock::Conversation;
use pam_client::{Context, Flag, Session};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ws::websocket::{Message, Opcode, WebSocket};

use crate::pty::{pty_fork, Fork};

pub async fn start<T: AsyncRead + AsyncWrite + Unpin>(
    mut ws: WebSocket<T>,
    user: User,
    mut context: Context<Conversation>,
) -> Result<(), Box<dyn Error>> {
    let session = context.open_session(Flag::NONE)?;
    let (child, mut master) = match pty_fork()? {
        Fork::Parent(child, master) => (child, master),
        Fork::Child => {
            let err = exec(user, session).unwrap_err();
            error!("exec: {:?}", err);
            exit(1);
        }
    };
    let _wait = Wait(child);

    let mut master_active = true;
    let mut close_send = false;

    let mut buf = vec![0; 1024];
    loop {
        tokio::select! {
            msg = ws.receive() => {
                match msg? {
                    Some(msg) => match msg.opcode() {
                        // xterm.js 使用 text 类型
                        Opcode::Text if master_active => master.write_all(msg.payload()).await?,
                        Opcode::Binary if master_active  => handle_resize(msg.payload(), master.as_raw_fd()),
                        Opcode::Close => {
                            if close_send {
                                break;
                            } else {
                                close_send = true;
                                ws.send(Message::close(&[])).await?;
                            }
                        },
                        _ => {}
                    }
                    None => break,
                }
            }
            n = master.read(&mut buf), if master_active => {
                match n {
                    Ok(n) if n > 0 => if !close_send {
                        ws.send(Message::text(&buf[..n])).await?;
                    }
                    _ => {
                        master_active = false;
                        if !close_send {
                            close_send = true;
                            ws.send(Message::close(&[])).await?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn exec(user: User, mut session: Session<Conversation>) -> nix::Result<Infallible> {
    let _ = session.putenv(&format!("HOME={}", user.dir.display()));
    if let Ok(v) = var("COLORTERM") {
        let _ = session.putenv(&format!("COLORTERM={}", v));
    }
    if let Ok(v) = var("TERM") {
        let _ = session.putenv(&format!("TERM={}", v));
    }

    setgid(user.gid)?;
    setuid(user.uid)?;
    chdir(&user.dir)?;
    let shell = CString::new(user.shell.to_str().unwrap().to_string()).unwrap();
    execve(
        &shell,
        &[&shell, &CString::new("--login").unwrap()],
        session.envlist().as_ref(),
    )
}

fn handle_resize(payload: &[u8], master: RawFd) {
    if payload.len() == 5 && payload[0] == 0xFF {
        let row = u16::from_le_bytes([payload[1], payload[2]]);
        let col = u16::from_le_bytes([payload[3], payload[4]]);
        if let Err(err) = resize(master, row, col) {
            error!("resize error: {:?}", err);
        }
    } else {
        warn!("unexpected binary message");
    }
}

// 调整终端大小
fn resize(fd: i32, row: c_ushort, col: c_ushort) -> io::Result<()> {
    let size = winsize {
        ws_row: row,
        ws_col: col,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let code = unsafe { ioctl(fd, TIOCSWINSZ, &size as *const _ as *const c_void) };
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

// drop 时杀掉并 wait 子进程
struct Wait(Pid);

impl Drop for Wait {
    fn drop(&mut self) {
        // 发送 SIGHUP
        // The SIGHUP ("hang-up") signal is used to report that the user’s terminal is disconnected
        debug!("send SIGHUP to {}", self.0);
        if let Err(err) = kill(self.0, Signal::SIGHUP) {
            error!("kill {}: {:?}", self.0, err);
        }

        let mut count = 0;
        loop {
            match waitpid(self.0, Some(WaitPidFlag::WNOHANG)) {
                Ok(status) if status.pid().is_some() => break,
                Ok(WaitStatus::StillAlive) => {
                    count += 1;
                    if count < 8 {
                        // 进程还没退出，继续等待
                        // FIXME sleep 会导致其它异步任务无法执行。除了新建一个线程 ，有没有不阻塞当前线程的方法？
                        sleep(Duration::from_millis(200));
                    } else {
                        error!("wait process {} timeout", self.0);
                        break;
                    }
                }
                Ok(_) => unreachable!(),
                Err(err) => break error!("wait process {}: {:?}", self.0, err),
            }
        }
    }
}
