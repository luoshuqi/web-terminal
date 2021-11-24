use std::io;
use std::mem::forget;
use std::os::unix::io::{AsRawFd, RawFd};
use std::pin::Pin;
use std::task::{Context, Poll};

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
    Parent(libc::pid_t, Master),
    Child,
}

// fork 进程，创建 pty 设备
// 父进程返回子进程 pid 和 pty 主设备
// 子进程复制 pty 从设备的文件描述符到标准输入、标准输出、标准错误
pub fn fork() -> io::Result<Fork> {
    unsafe {
        let master = open_master()?;
        match c!(fork()) {
            // 子进程
            0 => {
                //创建新会话
                c!(setsid());

                let slave = open_slave(master)?;
                let close = CloseFd(slave);
                close_fd(master);

                if dup_fd(slave, libc::STDIN_FILENO)? == slave
                    || dup_fd(slave, libc::STDOUT_FILENO)? == slave
                    || dup_fd(slave, libc::STDERR_FILENO)? == slave
                {
                    forget(close);
                }
                Ok(Fork::Child)
            }
            //父进程
            pid => Ok(Fork::Parent(pid, Master(AsyncFd::new(master)?))),
        }
    }
}

fn open_master() -> io::Result<RawFd> {
    unsafe {
        let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        if fd >= 0
            && libc::grantpt(fd) == 0
            && libc::unlockpt(fd) == 0
            && libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) == 0
        {
            Ok(fd)
        } else {
            if fd >= 0 {
                close_fd(fd);
            }
            Err(io::Error::last_os_error())
        }
    }
}

fn open_slave(master: RawFd) -> io::Result<RawFd> {
    unsafe {
        let path = libc::ptsname(master);
        if !path.is_null() {
            let fd = libc::open(path, libc::O_RDWR);
            if fd != -1 {
                return Ok(fd);
            }
        }
        return Err(io::Error::last_os_error());
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
                match read_fd(fd.as_raw_fd(), b) {
                    Ok(n) => {
                        buf.assume_init(n);
                        buf.advance(n);
                        Ok(())
                    }
                    Err(err) => Err(err),
                }
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
            match guard.try_io(|fd| write_fd(fd.as_raw_fd(), buf)) {
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

fn read_fd(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    unsafe { Ok(c!(read(fd, buf.as_mut_ptr() as _, buf.len())) as _) }
}

fn write_fd(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    unsafe { Ok(c!(write(fd, buf as *const _ as _, buf.len())) as _) }
}

fn close_fd(fd: RawFd) {
    unsafe {
        let ret = libc::close(fd);
        debug_assert_eq!(ret, 0);
    }
}

fn dup_fd(src: RawFd, dst: RawFd) -> io::Result<RawFd> {
    unsafe {
        c!(dup2(src, dst));
        Ok(dst)
    }
}

struct CloseFd(RawFd);

impl Drop for CloseFd {
    fn drop(&mut self) {
        close_fd(self.0)
    }
}
