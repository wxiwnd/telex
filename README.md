# Telex

一个相当轻量化的单机 Agent 用的邮箱。
不同 Harness 下的 Agent 各自跑在不同的会话里，需要互相留话、交接任务。Telex 把信落成文件，等对方下次醒来告诉它「你有 N 封未读」，剩下的它自己打开收件箱处理，逻辑和人类使用 Email 相似。提供一个单机不同 Harness（甚至是同 Harness）下 Agent 传递信息的方式。


## 为什么要做

不同 harness 之间没有现成的通信方式。你可以说把不同模型放进同一个 Harness 编排成 Subagent 或 Swarm，但模型在自己的 harness 里通常效果更好。

## Quick Start

```bash
cargo build --release

# 创建 XDG 目录、管理密码和第一个角色；设置 TELEX_SESSION_ID 时会自动绑定
TELEX_SESSION_ID=lead-1 ./target/release/tlx setup

./target/release/tlx role add worker@local --password-stdin

TELEX_SESSION_ID=lead-1 ./target/release/tlx send \
  --to worker@local --subject "flaky test" --body "Can you look?" --intent question

TELEX_SESSION_ID=worker-1 ./target/release/tlx login worker@local
# 记下配对码，在终端运行 tlx approve <配对码> --password-stdin
# 批准后 worker 的 hook 才会生效，就能用 inbox 和 read
```

接入 harness：

```bash
./target/release/tlx connect claude --write   # Claude Code
./target/release/tlx connect codex --write    # Codex，装完在 codex 里跑 /hooks 信任
./target/release/tlx connect dsh --write      # DSH
./target/release/tlx connect                  # 查看接入状态
```

测试：`cargo test`。

## 数据位置

路径按 XDG Base Directory 规范拆开：

| 内容 | 路径 |
|---|---|
| 配置 | `~/.config/telex/config.json` |
| 角色、邮件、已读/回执标记 | `~/.local/share/telex/` |
| 登录、token、待批申请、认领、唤醒记录 | `~/.local/state/telex/` |
| SQLite 索引 | `~/.cache/telex/index.sqlite3` |
| socket 和锁文件 | `$XDG_RUNTIME_DIR/telex/` |

如果未设置 XDG Base Directory 变量值，会使用默认值。

## 设计
整体架构如下图所示：

```
Claude Code / Codex / DSH
  ├── tlx CLI
  └── tlx hook
          │ Unix socket，NDJSON
          ▼
       maild
          ├── Store：角色、绑定、spool、marker、claim
          └── SQLite 索引：列表和线程查询
```


## 文档

功能设计 [docs/DESIGN.md](docs/DESIGN.md)。

名字来自老式电传网：每台机器有地址，消息异步送达，发的时候对面不一定在线。
