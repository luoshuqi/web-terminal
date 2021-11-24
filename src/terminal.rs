use std::error::Error;
use std::ffi::{c_void, CString};
use std::io;
use std::os::raw::c_ushort;
use std::os::unix::io::{AsRawFd, RawFd};
use std::ptr::null;
use std::thread::sleep;
use std::time::Duration;

use log::{debug, error, warn};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ws::websocket::{Message, Opcode, WebSocket};

use crate::pty::{self, Fork};

pub async fn start<T: AsyncRead + AsyncWrite + Unpin>(
    mut ws: WebSocket<T>,
    shell: impl ToString,
) -> Result<(), Box<dyn Error>> {
    let (child, mut master) = match pty::fork()? {
        Fork::Parent(child, master) => (child, master),
        _ => return exec(shell).map_err(Into::into),
    };
    let _child = ChildProcess(child);

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

fn exec(shell: impl ToString) -> io::Result<()> {
    let shell = CString::new(shell.to_string())?;
    unsafe {
        c!(execvp(shell.as_ptr(), null()));
    }
    Ok(())
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
    let size = libc::winsize {
        ws_row: row,
        ws_col: col,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        c!(ioctl(
            fd,
            libc::TIOCSWINSZ,
            &size as *const _ as *const c_void
        ));
    }
    Ok(())
}

// drop 时杀掉并 wait 子进程
struct ChildProcess(libc::pid_t);

impl Drop for ChildProcess {
    fn drop(&mut self) {
        unsafe {
            // 发送 SIGHUP
            // The SIGHUP ("hang-up") signal is used to report that the user’s terminal is disconnected
            debug!("send SIGHUP to {}", self.0);
            if libc::kill(self.0, libc::SIGHUP) == -1 {
                error!("kill {}: {:?}", self.0, last_os_error());
            }

            let mut status = 0;
            let mut count = 0;
            loop {
                match libc::waitpid(self.0, &mut status as *mut _, libc::WNOHANG) {
                    pid if pid == self.0 => {
                        debug!("process {} exit with status {}", self.0, status);
                        break;
                    }
                    0 => {
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
                    -1 => break error!("wait process {}: {:?}", self.0, last_os_error()),
                    _ => unreachable!(),
                }
            }
        }
    }
}

fn last_os_error() -> io::Error {
    io::Error::last_os_error()
}
