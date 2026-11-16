# Telex

给本机 agent 用的邮箱。不同 harness 下的 agent 各自跑在不同的会话里，需要互相留话、交接任务。
Telex 把这些信落成文件，等对方下次醒来告诉它「你有 N 封未读」，剩下的它自己打开收件箱处理，跟人用邮件差不多。

P1 已经实现：Rust 写的 `tlx`，本地 maild、角色与登录、一对一绑定、收发、已发送和收件人状态、启动角标和唤醒。旧会话被改绑后返回 `session_revoked`；唤醒失败会记进 `tlx doctor`。邮件列表和线程走可重建的 SQLite 索引。设计和未决项在 `docs/DESIGN.md`。

名字来自老式电传网：每台机器有地址，消息异步送达，发的时候对面不一定在线。


## 为什么要做

不同 harness 下的 agent 之间没有现成的通信方式，而 agent 之间需要交流协同工作。
可以把不同模型放进同一个 harness 编排成 subagent 或 swarm，但模型在自己的 harness 里效果最好，GPT 和 Claude 换到别人的 harness 里也别扭。

Telex 让不同 harness 下的 agent 用类似 email 的方式互相通信。

## 跑起来

```bash
cargo build --release
./target/release/tlx init --password-stdin            # 设置管理密码
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

测试：`cargo test`。数据默认在 `~/.telex`，可以用 `TELEX_HOME` 指到别处。

## 文档

功能设计都在 [docs/DESIGN.md](docs/DESIGN.md) 这一份文件里，行为规则、失败表现和分期都在那儿。实现细节放在本地的 `.work/IMPL.md`，不进 git。

两份前作评估在 `.work/` 里，看的是 mcp_agent_mail_rust 和朋友写的 leavelet/agent-mail。结论都是不直接采用，借了几处设计。`.work/` 和 `AGENTS.md` 都是本地工作文件，不进 git。
