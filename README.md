# web-terminal

#### 介绍
用 Rust 编写的网页终端。

实现方式：用 WebSocket 连接前端 [xterm.js](https://xtermjs.org/) 和后端 bash 进程。


#### 使用说明

```shell
web-terminal -b 127.0.0.1:8888 -u demo -p demo
```

-b 指定地址，-u 指定登录用户名，-p 指定登录密码。

[点此查看演示](https://demo.trait.pub/web-terminal/) （用户名密码均为 `demo`）

[下载（Linux x64）](https://gitee.com/luoshuqi/web-terminal/attach_files/889494/download/web-terminal)
