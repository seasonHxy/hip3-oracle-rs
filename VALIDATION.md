# Rust 0.1.0 验收记录

日期：2026-10-24（Asia/Shanghai）。项目：`seasonHxy/hip3-oracle-rs`。Rust 实现已从原 Python 仓库拆分为独立项目，以下历史证据记录来自拆分前的本机验收；拆分后复验记录见下节。测试平台：macOS / Darwin arm64，Rust/Cargo 1.96.0，Python 3.12 + 官方 hyperliquid-python-sdk 0.24.0。

本次结论：原生 Rust 实现已通过本机编译、离线签名兼容和本地故障测试，可进入目标 DEX 的 Testnet 验收。尚未通过主网生产发布验收；没有发送任何链上交易或部署线上服务。此项目为后台服务，未发布生产网站，网页 SEO 验收不适用。

| 检查 | 实际结果 | 证据 |
|---|---|---|
| Rust 测试 | 15 项通过：6 个模块测试、2 个 CLI 集成测试、7 个安全与兼容测试 | `validation/rust-tests.log` |
| 原生签名 | 对照官方 SDK 实际 setOracle 构造，四组向量（Mainnet/Testnet × 有/无 mark）逐项匹配 action hash、r/s/v | `tests/generate_vectors.py`、`tests/sdk_vectors.json`、`tests/safety.rs` |
| 故障注入 | 超时、API 拒绝、readback 不一致保留 Pending；重启不重发；reconcile 只读；暂停、跳价阻止发送 | `src/engine.rs` 模块测试 |
| 行情请求 | 本地 HTTP 测试验证精确字段解析、429 有限重试、禁止重定向、总超时 | `src/transport.rs` 模块测试 |
| 状态存储 | 独占锁、scope 隔离、暂停落盘、损坏状态拒绝 | `tests/safety.rs` |
| 运维进程 | preflight 不写盘，dry-run 明确 simulated；ready/metrics 和 SIGTERM 正常退出 | `tests/cli.rs` |
| Clippy / 格式 | all-targets + `-D warnings` 通过；cargo fmt 和 git diff --check 通过 | `validation/clippy.log` |
| Release | 本机 release 编译通过；`--version`、preflight、once 实测通过，示例输出 demo:AAPL=230.2 | 下方二进制记录 |
| 旧版回归 | 原 Python 测试 102 项通过，包含官方 SDK 兼容测试 | `validation/python-regression.log` |
| 部署模板 | Docker Compose config 语法验证通过；镜像构建未完成 | `compose.yaml`、`validation/docker-build.log` |
| 公开行情线上预检 | Binance/OKX/KuCoin 三个来源在本网络超时，Rust 正确以 insufficient source quorum 拒绝整批 | `validation/public-preflight.log` |
| 密钥保护 | 新生成专用本地测试钱包，.env 为 0600、Git/Docker 忽略；未在日志或版本文件保存私钥 | `.gitignore`、`.dockerignore` |

Release 二进制：`target/release/hip3-oracle-rs`（本机 macOS arm64，不是 Linux 发布包）。SHA-256：`28f9093e084490bb1a06818c06e8b1eda049c0ccb518ec3464770709be8f8393`。重新编译时哈希可能改变；交付应记录自己的构建环境、版本和镜像 digest。

测试钱包公开地址：`0xfF98d881617270C045A57fdf5f375536753445d0`。随机生成、无资金、未取得任何 DEX 授权。私钥只存在 Git 忽略的本地 `.env`。官方 SDK 签名向量中的固定 `[1;32]` 是公开离线测试值，与本地钱包无关。

## 独立仓库迁移复验（2026-10-24）

- 新仓库：`seasonHxy/hip3-oracle-rs`，分支 `dev`；采用独立历史，不包含原 Python 项目的源码或历史提交。
- Rust 源码、配置、Dockerfile 和 CI 均放在项目根目录；CLI 默认配置改为 `config/example.json`。
- 在独立目录结构下重新执行 15 项 Rust 测试、Clippy all-targets 零警告和格式检查，全部通过。证据：`validation/standalone-tests.log`、`validation/standalone-clippy.log`。
- Release 编译及不带 `--config` 参数的默认 preflight/once 实测通过；输出明确为模拟价格，未提交链上交易。
- 此次 macOS arm64 Release SHA-256：`70761d96a522ee7c5d8b01fe17fe48a31fae83e20a4634c341b1d784d40fbd95`。前述旧二进制哈希保留为拆分前历史记录。
- 原项目的 Docker 网络失败与公网行情超时是历史证据，本次目录迁移没有将它们改记为通过。
- 本地测试 `.env`、运行状态和构建输出均未纳入 Git；新仓库的 Git/Docker 忽略规则同时覆盖 `.env.*`。

## 复现

从仓库根目录运行：

```sh
cargo +1.96.0 fmt --check
cargo +1.96.0 clippy --locked --all-targets -- -D warnings
cargo +1.96.0 test --locked
cargo +1.96.0 build --release --locked
target/release/hip3-oracle-rs --config config/example.json preflight
target/release/hip3-oracle-rs --config config/example.json once
```

四份配置（静态、本机公共行情、容器、Testnet 占位模板）均已执行 validate。CI 另配置 Linux/macOS 测试、官方 SDK 向量再生成比对、Linux 容器离线 smoke；CI 执行结果以新仓库 Actions 记录为准，不将配置文件本身视为运行成功。

本机 crates.io 大型索引下载反复超时，实际构建使用临时本地缓存代理，通过系统 curl 从官方 index.crates.io/static.crates.io 获取原始内容，Cargo 继续执行包 checksum 校验；临时代理配置位于 /tmp，不属于项目。Cargo.lock 保留标准官方 registry 来源。没有引入镜像源或私有注册表要求。

## 尚未完成的发布项

- **P0 / Testnet 联调**：需操作员提供自己的 DEX、完整资产与 szDecimals，并授权测试钱包 setOracle。首次 bootstrap 后完成签名发送、完整回读和 updater 权限验证；当前没有链上成功记录。
- **P0 / 真实数据定义**：需明确目标市场、指数/计价货币、许可供应商及实际独立性。公共 BTC/USDT 配置只是采集调试示例；不能作为 AAPL 或 USD 指数的生产配置。
- **P1 / 网络与容器**：本机公网行情请求超时；Docker 基础镜像拉取超时（registry-1.docker.io），未完成镜像编译与容器 smoke。需在能访问供应商与镜像仓库的部署环境复验，不能把超时写成供应商数据有效或无效。
- **P1 / 长时间与恢复验收**：在专用 Testnet DEX 连续运行，演练断网、供应商故障、重启、SIGKILL、磁盘写失败、暂停恢复和 Pending 核对，确认业务可接受的更新间隔、陈旧阈值和告警通知。
- **P1 / 发布环境**：正式 Linux 构建、依赖漏洞与镜像扫描、secret manager 注入、专用 updater 权限、唯一 writer 与 fencing 策略、持久磁盘及审计归档尚需在目标环境验收。本机文件锁只覆盖同一主机同一路径，禁止据此启用多活。
- **P1 / 产品风险**：独立 mark/perp 输入、休市/停盘流程与价格参考调整需要按产品定义验收。本程序只发布标准 HIP-3 setOracle，不实现 HIP-3* star action，也不自动调用 haltTrading。

上述缺项未被模拟测试替代，不构成主网已可用的声明。
