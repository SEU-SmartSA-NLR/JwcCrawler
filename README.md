# JwcCrawler

本仓库是原 OpenJWC 项目的一个组件，用来爬取教务处官网的信息并保存到 JSON 文件中。本分支经修改，适用于sega项目nlr业务。

## NLR sidecar 模式

单 worker 监听版本化 spool：

```bash
cargo run --bin nlr_sidecar -- --spool-dir /var/run/nlr-b5-jwc
```

使用 `--once` 可处理当前队列后退出。spool 必须是 sidecar 与 NLR 共用的本机私有目录，worker 会拒绝符号链接或非当前用户目录并收紧为 `0700`。worker 只接受 `schema_version=1`、固定 `seu-jwc` 来源、最长 120 秒 deadline、日期与 page/detail/total 三类硬预算；crawler 串行访问，使用跨重启限速状态，并应用一次受预算约束的重试、逐跳固定目标校验、响应类型/大小限制和有上限的原子 JSON 输出。启动时会把中断的 `.running` job 转成稳定失败并清理未发布 items。结果 manifest 绑定原始 job SHA-256；失败只写稳定 warning code，部分抓取写 `partial`，不复用旧输出。附件仅保留受控 HTTPS 引用，不会下载。

## 许可证

MIT License
