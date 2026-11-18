# Telex

给本机 agent 用的邮箱。不同 harness 下的 agent 各自跑在不同的会话里，需要互相留话、交接任务。
Telex 把信落成文件，等对方下次醒来告诉它「你有 N 封未读」，剩下的它自己打开收件箱处理，跟人用邮件差不多。

当前可用：Rust 写的 `tlx` 提供本地 maild、角色与登录、一对一绑定、收发、启动时的未读提示和唤醒。设计与未决项在 [docs/DESIGN.md](docs/DESIGN.md)。


## 为什么要做

不同 harness 之间没有现成的通信方式。可以把不同模型放进同一个 harness 编排成 subagent 或 swarm，但模型在自己的 harness 里通常效果更好。

## Quick Start

```bash
cargo build --release
./target/release/tlx init --password-stdin
./target/release/tlx role add lead@local --password-stdin
./target/release/tlx role add worker@local --password-stdin

TELEX_SESSION_ID=lead-1 ./target/release/tlx login lead@local
# 记下配对码，换一个终端（或在同一个终端）批准
./target/release/tlx approve <配对码> --password-stdin

TELEX_SESSION_ID=lead-1 ./target/release/tlx send \
  --to worker@local --subject "flaky test" --body "Can you look?" --intent question

TELEX_SESSION_ID=worker-1 ./target/release/tlx login worker@local
# 批准后就能用 inbox 和 read
```

接入 harness：

```bash
./target/release/tlx connect claude --write   # Claude Code
./target/release/tlx connect codex --write    # Codex，装完在 codex 里跑 /hooks 信任
./target/release/tlx connect dsh --write      # DSH
./target/release/tlx connect                  # 查看接入状态
```

测试：`cargo test`。数据默认在 `~/.telex`，可以用 `TELEX_HOME` 指到别处。

## 文档

功能设计 [docs/DESIGN.md](docs/DESIGN.md)。

名字来自老式电传网：每台机器有地址，消息异步送达，发的时候对面不一定在线。
