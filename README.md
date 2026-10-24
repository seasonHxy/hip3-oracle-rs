# HIP-3 Oracle · Rust

独立的原生 Rust 实现，无 Python 运行时依赖。独立仓库 `seasonHxy/hip3-oracle-rs`，项目根目录包含生产部署所需的持久化、风控、监控与运维流程；真实 DEX 的 Testnet 长时间运行、权限和恢复演练仍是正式主网上线前的验收项。没有自动部署或发送主网交易。

## 快速运行

要求 Linux/macOS、Rust 1.96.0（目录内 toolchain 已固定）。从仓库根目录运行：

```sh
cargo +1.96.0 build --release --locked
cargo +1.96.0 test --locked
target/release/hip3-oracle-rs --config config/example.json validate
target/release/hip3-oracle-rs --config config/example.json preflight
target/release/hip3-oracle-rs --config config/example.json once
target/release/hip3-oracle-rs --config config/example.json run
```

`example.json` 是明确标记为 simulated 的静态行情示例。`public-btc.dry-run.json` 使用 Binance、OKX、KuCoin 的 BTC/USDT 公共现货行情，无 API Key。此配置仅用于网络/采集联调：USDT 报价不等同于 USD 指数，`demo:BTC` 和精度为示例，不能直接用于真实 DEX。行情 API 在部分地区可能受限；失败、过期或三组 quorum 不足会拒绝整批，不使用缓存假装新报价。

```sh
target/release/hip3-oracle-rs --config config/public-btc.dry-run.json once
```

## 行为与安全边界

- 使用 `rust_decimal`，不以 f64 计算价格。范围 `10^-12 <= price <= 10^12`，权重范围 `10^-6..10^6`；价格线格式支持整数豁免、5 位有效数字、`6-szDecimals` 小数与银行家舍入。超界拒绝。
- 每来源有限重试（网络、429、5xx，最多两次）、总超时、响应体 2 MB 上限、共享来源最小请求间隔。拒绝 HTTP 重定向。只接受 HTTPS；URL 的 `{symbol}`/`{coin}` 使用 symbols 映射并编码；凭据通过 header 的 `${ENV}` 注入。
- JSON 支持点路径和 JSON Pointer；必须配置行情产生时间的 `timestampPath`，支持整数毫秒/秒。bid/ask 配对检查；时间过期、未来时间、spread 超限拒绝报价。没有交易日历，marketStatus 字段/路径由供应商配置；默认 unknown，feed 默认阻止 unknown。
- 多接口同组只占一个投票权，组权重取成员最大值；组内 weighted median，再组间 MAD 和 weighted median。过滤前后同时检查来源和独立组 quorum、单组权重上限（严格小于 50%）；IQR confidence、市场状态、跳价任一失败拒绝全批。至少三个独立组。
- 异步并发采集（最多同时 16 个 feed，每 feed 最多 16 来源）；全量资产通过后才构建排序后的 `perpDeploy.setOracle`。`externalPerpPxs` 使用聚合指数；默认 `markPxs=[]`，可显式提供一组聚合价，不伪造两个独立 mark。需要独立 perpetual mark/EMA 的产品必须增加专用管线。
- 原生 MessagePack → Keccak → EIP-712 → secp256k1 签名；结构体字段顺序固定，使用官方 Python SDK 0.24.0 离线向量验证。网络决定官方 endpoint 和签名域，禁止通过配置覆盖。
- Live 每周期先核对 DEX 的完整 universe 和 szDecimals，再采集最新价格。发送前持久化 nonce、action、Pending，fsync 临时文件、原子替换、fsync 父目录；落盘后再次检查真实上游时间。
- 仅发送一次 `/exchange`，只重试 info 回读。API ok 且全资产 oraclePx 一致才提交参考价；超时、拒绝、损坏响应、回读不一致均保留 Pending，后续周期及重启都不重发。保守地把明确拒绝也留待人工核对。
- 读取价格一致性不是交易包含证明；相同价格可由旧请求或其他 writer 产生。运维必须保证唯一 updater。
- 网络、模式、DEX、Rust schema 各自隔离状态；不自动读取 Python 状态。每个周期和运维修改持有同一路径文件锁。仅支持单主机本地可靠磁盘；不同主机/不同状态路径无法相互隔离，不能多活运行。不要在 NFS 上依赖这些保证。
- 成功确认/人工核对后持久化冷却时间，进程重启仍保留至少 2.5 秒限制。系统时间回退时阻止发布；部署主机应同步时钟。
- JSON 日志输出 stderr；报价、聚合结果、action、nonce、运维理由写 JSONL 审计并同步磁盘，每 10 MB 轮转、保留五份。需要外部长期归档和磁盘告警。私钥和认证 header 不进入日志。

## Testnet → Mainnet

1. 复制 `config/testnet.example.json` 为 `config/testnet.local.json`。替换真实 DEX、**所有**合约及 szDecimals、供应商 URL、symbols、时间字段。`.example` 是占位符。独立性按底层交易场所评估，不能按域名数量计算。
2. 由你控制的 deployer 给专用测试钱包授予该 DEX 的 `setOracle` 权限。普通交易 API agent key 不自动拥有 deployer 权限；测试钱包也不能写入他人的 DEX。测试网钱包和行情供应商 API Key 是两种不同凭据。
3. 使用密钥管理器注入 `HIP3_PRIVATE_KEY`、供应商 token。仓库不会提供公共可复用私钥。本地调试生成的钱包保存在 git 忽略的 `.env`，权限 0600，没有资金，只用于 Testnet；不要提交、转账或在主网复用。程序不自动加载 `.env`。其他环境需要自行生成专用测试钱包；`.env` 不随仓库分发。
4. 无私钥建立链上参考并检查完整数据管线：

```sh
target/release/hip3-oracle-rs --config config/testnet.local.json validate
target/release/hip3-oracle-rs --config config/testnet.local.json bootstrap --reason 'initial testnet reference'
target/release/hip3-oracle-rs --config config/testnet.local.json preflight
# shell 环境由你的密钥管理器注入；本地测试钱包可使用 set -a; . .env; set +a
export HIP3_ENABLE_LIVE=YES
target/release/hip3-oracle-rs --config config/testnet.local.json once --live
target/release/hip3-oracle-rs --config config/testnet.local.json run --live
```

首次必须 bootstrap；已有参考不允许覆盖，跳价熔断不能靠重启解除。主网使用独立配置 `network=mainnet`、独立钱包并额外设置 `HIP3_ENABLE_MAINNET=YES`。不要在仅为 crypto 配置的行情模板上替换成股票代码后直接上线；股票还需要许可行情、开休市策略、指数定义和停盘流程。

## 运维

`run` 启动配置中的 HTTP 监控：`/livez` 进程存活；`/readyz`、`/healthz` 最近周期通过且未过期时 200，否则 503；`/metrics` 提供模式、周期/失败数、最后成功时间。dry-run 的健康只表示模拟成功。监控绑定默认本机，跨容器抓取时配置 `monitoring.host=0.0.0.0`，在网络层限制访问。Prometheus 告警模板在 `deploy/alerts.yml`，须接入自己的通知接收人。

```sh
hip3-oracle-rs --config /etc/hip3-oracle/live.local.json status
hip3-oracle-rs --config /etc/hip3-oracle/live.local.json pause --reason 'provider incident'
hip3-oracle-rs --config /etc/hip3-oracle/live.local.json resume --pause-id '<pause 输出 ID>' --reason 'verified recovery'
hip3-oracle-rs --config /etc/hip3-oracle/live.local.json reconcile --reason 'exclusive updater; investigated request and verified complete oraclePx'
```

pause 在下次周期阻止发布；若周期持锁，运维命令立即失败，等本周期结束重试。resume 不清除 Pending 或参考价。reconcile 仅查询 info，必须完整匹配待核对批次才能解除 Pending，不发送交易。不一致时保留文件、审计和日志，停止所有 updater，调查链上状态；没有“强制清空 pending”命令。

SIGTERM/SIGINT 停止安排下一周期，等待当前周期确认或保存 Pending。监控服务器异常会退出进程。Docker 停机宽限 60 秒；大量 feed 的排队/采集可能需要提高宽限。停止喂价并不等于停止交易，HIP-3 可能回退到本地 mark；市场维护由独立、显式授权的 deployer 运维流程处理，不自动调用 haltTrading。

## 部署

```sh
docker compose up --build -d  # 默认仅模拟
```

镜像非 root，Compose 根文件系统只读，持久状态卷、cap_drop、健康检查已配置。Live 挂载只读配置到 `/app/config/live.local.json`，stateFile 设 `/app/state/oracle.json`，通过 secret manager 注入环境并覆盖命令为 `--config /app/config/live.local.json run --live`。发布镜像应在自己的 registry 固定 digest 并做漏洞扫描。

systemd 模板：`deploy/hip3-oracle.service`；创建同名系统用户，配置放 `/etc/hip3-oracle/live.local.json`，stateFile 必须指向 `/var/lib/hip3-oracle/oracle.json`，`secrets.env` 权限 0600。不要让 Python 版和 Rust 版同时写同一个 DEX。

验证与未完成的上线验收项见 `VALIDATION.md`。本服务不是网站，本次未发布生产网页。

## 协议和行情来源

- [HIP-3 deployer actions](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/hip-3-deployer-actions)
- [官方 SDK signing.py](https://github.com/hyperliquid-dex/hyperliquid-python-sdk/blob/master/hyperliquid/utils/signing.py)
- [Binance Spot REST market data](https://developers.binance.com/docs/binance-spot-api-docs/rest-api/market-data-endpoints)
- [OKX API](https://app.okx.com/docs-v5/en/)
- [KuCoin ticker](https://www.kucoin.com/docs-new/rest/spot-trading/market-data/get-ticker)
