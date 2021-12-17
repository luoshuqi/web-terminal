# web-terminal

#### 介绍
用 Rust 编写的网页终端。

实现方式：用 WebSocket 连接前端 [xterm.js](https://xtermjs.org/) 和后端 shell 进程。

#### 使用说明

```shell
web-terminal -b 127.0.0.1:8888
```

-b 选项指定地址。

#### 用户验证

用户验证使用 `PAM`, service name 为 `web-terminal`。

用户可登录的前提是设置了密码，shell 不为 `false` 或 `nologin`。

如果以 root 权限执行本程序，所有可登录系统的用户都可以登录。

如果以普通权限执行，只有执行用户可以登录。



[点此查看演示](https://demo.trait.pub/web-terminal/) （用户名密码均为 `demo`）

[下载（Linux x64）](https://gitee.com/luoshuqi/web-terminal/attach_files/889494/download/web-terminal)
