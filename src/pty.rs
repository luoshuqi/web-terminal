use std::io;
use std::mem::forget;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::prelude::IntoRawFd;
use std::pin::Pin;
use std::process::exit;
use std::task::{Context, Poll};
use std::thread::sleep;
use std::time::Duration;

use log::error;
use nix::fcntl::{fcntl, open, OFlag, F_SETFL};
use nix::libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::pty::{grantpt, posix_openpt, ptsname_r, unlockpt, PtyMaster};
use nix::sys::signal::{kill, Signal};
use nix::sys::stat::Mode;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{close, dup2, fork, read, setsid, write, ForkResult, Pid};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

macro_rules! ready {
    ($e:expr) => {
        match $e {
            Poll::Ready(t) => t,
            Poll::Pending => return Poll::Pending,
        }
    };
}

pub enum Fork {
    Parent(Child, Master),
    Child,
}

// fork 进程，创建 pty 设备
// 父进程返回子进程 pid 和 pty 主设备
// 子进程复制 pty 从设备的文件描述符到标准输入、标准输出、标准错误
pub fn pty_fork() -> io::Result<Fork> {
    let master = open_master()?;
    match unsafe { fork()? } {
        ForkResult::Parent { child } => Ok(Fork::Parent(
            Child(child),
            Master(AsyncFd::new(master.into_raw_fd())?),
        )),
        ForkResult::Child => match setup_slave(&master) {
            Ok(()) => Ok(Fork::Child),
            Err(err) => {
                error!("setup slave: {:?}", err);
                exit(1);
            }
        },
    }
}

fn open_master() -> nix::Result<PtyMaster> {
    let master = posix_openpt(OFlag::O_RDWR | OFlag::O_NOCTTY)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    fcntl(master.as_raw_fd(), F_SETFL(OFlag::O_NONBLOCK))?;
    Ok(master)
}

fn setup_slave(master: &PtyMaster) -> nix::Result<()> {
    setsid()?;

    let path = ptsname_r(master)?;
    let slave = Slave(open(
        path.as_str(),
        OFlag::O_RDWR,
        Mode::S_IRUSR | Mode::S_IWUSR,
    )?);
    let fd = slave.0;
    dup2(fd, STDIN_FILENO)?;
    dup2(fd, STDOUT_FILENO)?;
    dup2(fd, STDERR_FILENO)?;
    if fd == STDIN_FILENO || fd == STDOUT_FILENO || fd == STDERR_FILENO {
        forget(slave);
    }
    Ok(())
}

struct Slave(RawFd);

impl Drop for Slave {
    fn drop(&mut self) {
        let _ret = close(self.0);
        debug_assert!(_ret.is_ok());
    }
}

pub struct Master(AsyncFd<RawFd>);

impl AsRawFd for Master {
    #[inline]
    fn as_raw_fd(&self) -> RawFd {
        *self.0.get_ref()
    }
}

impl AsyncRead for Master {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.0.poll_read_ready(cx))?;
            match guard.try_io(|fd| unsafe {
                let b = &mut *(buf.unfilled_mut() as *mut _ as *mut [u8]);
                let n = read(fd.as_raw_fd(), b)?;
                buf.assume_init(n);
                buf.advance(n);
                Ok(())
            }) {
                Ok(result) => return Poll::Ready(result),
                Err(_) => continue,
            }
        }
    }
}

impl AsyncWrite for Master {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = ready!(self.0.poll_write_ready_mut(cx))?;
            match guard.try_io(|fd| write(fd.as_raw_fd(), buf).map_err(Into::into)) {
                Ok(result) => return Poll::Ready(result),
                Err(_) => continue,
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub struct Child(Pid);

impl Drop for Child {
    fn drop(&mut self) {
        // 发送 SIGHUP
        // The SIGHUP ("hang-up") signal is used to report that the user’s terminal is disconnected
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
