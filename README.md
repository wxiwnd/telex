# Telex

一个相当轻量化的单机 Agent 用的邮箱。
不同 Harness 下的 Agent 各自跑在不同的会话里，需要互相留话、交接任务。Telex 把信落成文件，等对方下次醒来告诉它「你有 N 封未读」，剩下的它自己打开收件箱处理，逻辑和人类使用 Email 相似。提供一个单机不同 Harness（甚至是同 Harness）下 Agent 传递信息的方式。


## 为什么要做

不同 harness 之间没有现成的通信方式。你可以说把不同模型放进同一个 Harness 编排成 Subagent 或 Swarm，但模型在自己的 harness 里通常效果更好。

## 安装

支持 macOS 和 Linux。安装脚本从 GitHub Releases 下载对应平台的二进制：

```bash
curl -fsSL https://github.com/wxiwnd/telex/releases/latest/download/install.sh | sh
```

默认装到 `~/.local/bin/tlx`。如果这个目录不在 `PATH` 里，安装脚本会打印提示。

固定版本：

```bash
curl -fsSL https://github.com/wxiwnd/telex/releases/latest/download/install.sh \
  | sh -s -- --version 0.1.0
```

源码开发：

```bash
cargo install --path . --locked
# 或
cargo build --release
```

## Quick Start

```bash
# 创建 XDG 目录、管理密码和第一个角色
tlx setup
tlx role add worker@local --password-stdin

# 在 lead agent 所在的 harness 会话里
tlx login lead@local
# 记下配对码，在终端运行 tlx approve <配对码> --password-stdin

tlx send \
  --to worker@local --subject "flaky test" --body "Can you look?" --intent question

# 在 worker agent 所在的 harness 会话里
tlx login worker@local
# approve 之后 hook 才生效，就能用 inbox 和 read
```

接入 harness：

```bash
tlx connect claude          # Claude Code
tlx connect codex           # Codex，装完在 codex 里跑 /hooks 信任
tlx connect dsh             # DSH
tlx connect agy             # Antigravity CLI
tlx connect                  # 查看接入状态
```

人查看某个角色的收件箱：

```bash
tlx admin inbox worker@local --password-stdin
tlx admin read worker@local <MESSAGE_ID> --password-stdin
# 默认只读，不写已读回执；要标记已读时加 --mark-seen
```

角色生命周期：

```bash
tlx role update worker@local --display "Worker" --password-stdin
tlx role delete worker@local --password-stdin   # 归档，保留邮件
tlx role list --all --password-stdin             # 查看归档
tlx role restore worker@local --password-stdin
tlx role purge worker@local --yes --password-stdin  # 永久删除
```

测试：`cargo test`。CI 在 push/PR 时跑 fmt、clippy 和 Linux 测试；tag `v*` 时构建并发布二进制，见 `.github/workflows/`。

## 数据位置

路径按 XDG Base Directory 规范拆开：

| 内容 | 路径 |
|---|---|
| 配置 | `~/.config/telex/config.json` |
| 角色、邮件、已读/回执标记 | `~/.local/share/telex/` |
| 登录、token、待批申请、认领、会话交接 | `~/.local/state/telex/` |
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
