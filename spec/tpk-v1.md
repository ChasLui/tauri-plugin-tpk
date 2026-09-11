# Tauri TPK（类 Godot PCK）完整方案

版本：1.0.0  
状态：最终态契约（格式 / API / 状态机一经落地不得做不兼容变更）  
许可目标：实现依赖优先 MIT / Apache-2.0 / BSD-2/3；禁止把 GPL 带进应用运行时

---

## 0. 以终为始：上线后系统长什么样

用户安装的是「壳 + 保底内容」。之后绝大多数内容更新只下载 `.tpk`，不必替换可执行文件。

最终态必须同时具备，而不是留到二期再改协议：

1. 多层叠加：Embedded → Seed Base（安装包内）→ Downloaded Base → Patches → DLC → Mods  
2. 文件级增量 + 大文件二进制 delta + 删除墓碑  
3. 签名清单 + 包签名 + 防重放 + `min_shell`  
4. 三态指针：staged / booting / committed，坏包永不砖机  
5. 渠道、灰度、强制壳升级  
6. 官方 `tauri-plugin-updater` 只负责原生壳  
7. 同一套格式用于桌面与移动；WebView origin 永不改变  
8. Rust 与 WebView 走同一 resolver 读资源  

未实现的代码可以分批写，但 **磁盘格式、清单字段、命令名、状态机** 第一天按本文冻结。

---

## 1. 角色与数据流（终态）

```
构建机
  dist/ + extra/dlc + extra/mods-policy
       │  tpk pack / tpk delta / tpk sign
       ▼
  artifacts/
    base-core-1.0.0.tpk
    patch-core-1.0.3.tpk
    dlc-maps-2.0.0.tpk
    channel-stable.json
    channel-stable.json.minisig
       │  上传 CDN
       ▼
客户端
  启动 → PackStore::boot() 处理 staged/booting
       → Context.set_assets(PackAssets)
       → WebView 以 tauri:// 读叠加后的文件
  空闲 → check → download → stage
  前端壳就绪 → notify_ready → commit
  壳过旧 → 拒绝 pack，改走官方 updater
```

两条更新通道永远分开：

| 通道 | 产物 | 触发 |
|---|---|---|
| Shell | 官方 updater 整包（.app.tar.gz / msi.zip / AppImage.tar.gz） | Rust、权限、插件、sidecar、WebView 能力、CSP 策略结构变化 |
| Content | `.tpk` + 渠道清单 | JS/CSS/HTML、文案、图片、关卡、配置、可热替换资源 |

前端禁止配置更新 URL 与公钥。

---

## 2. 仓库与 crate 终态

```
tpk/
  crates/
    tpk-format/          # 读写、校验、规范化路径
    tpk-delta/           # bsdiff + zstd，纯库
    tpk-resolve/         # 叠加、墓碑、delta 应用、缓存
    tpk-store/           # 磁盘布局、三态、拉黑
    tpk-client/          # 清单拉取、下载、重试
    tpk-cli/             # pack / inspect / sign / channel
    tauri-plugin-tpk/    # Assets 替换 + commands + events
  packages/
    tauri-plugin-tpk-api/  # TS guest
  examples/
    shell-app/
  spec/
    tpk-v1.md            # 本文
    json-schema/
      pack-manifest.schema.json
      channel-manifest.schema.json
      store-state.schema.json
```

应用侧最小接入：

```
src-tauri/
  tauri.conf.json          # plugins.tpk
  capabilities/main.json   # tpk:default
  src/lib.rs               # set_assets + plugin 最先注册
```

---

## 3. 包格式 TPK/1（冻结）

### 3.1 容器

- 外层：ZIP（stored 或 deflate 均可；推荐 stored + 内层 blob 自压缩，便于随机读）
- 中央目录必须存在
- 根目录文件：

```
tpk-manifest.json
tpk-manifest.json.minisig      # 可选，推荐同时发分离签名
blobs/<sha256>
blobs/<sha256>.zst
```

- 文件名魔法：扩展名 `.tpk`
- MIME：`application/vnd.tauri.tpk+zip`

不使用自定义二进制头作为唯一真相，ZIP+JSON 便于检视与跨语言；校验以清单为准。

### 3.2 `tpk-manifest.json`

```json
{
  "spec": "tpk/1",
  "kind": "base | patch | dlc | mod",
  "id": "core",
  "version": "1.0.3",
  "version_code": 10003,
  "min_shell": "2.3.0",
  "max_shell": null,
  "parent": {
    "id": "core",
    "version": "1.0.0",
    "version_code": 10000,
    "manifest_sha256": "hex64"
  },
  "created_at": "2026-09-08T15:00:00Z",
  "channel": "stable",
  "policies": {
    "can_override": ["**"],
    "cannot_override": [],
    "trusted": true
  },
  "entries": [
    {
      "path": "/index.html",
      "op": "full",
      "size": 4096,
      "sha256": "hex64",
      "blob": "blobs/<sha256>.zst",
      "encoding": "zstd"
    },
    {
      "path": "/assets/big.bin",
      "op": "delta",
      "size": 1048576,
      "sha256": "hex64",
      "blob": "blobs/<delta-sha256>.zst",
      "encoding": "zstd+bsdiff",
      "delta_base_sha256": "hex64"
    },
    {
      "path": "/old.css",
      "op": "delete"
    }
  ]
}
```

字段规则：

- `spec` 必须为 `tpk/1`。未知 spec 拒绝加载。
- `kind` 四选一，禁止扩展时改旧包含义；新 kind 必须升 spec。
- `id`：`[a-z0-9][a-z0-9-]{0,62}`。
- `version`：SemVer。`version_code`：单调正整数，比较只用它。
- `parent`：`base` 必须为 `null`；`patch` 必须存在；`dlc`/`mod` 可空（叠加在解析结果上）。
- `path`：POSIX，必须以 `/` 开头，UTF-8 NFC，禁止 `\`、`.`、`..`、空段、盘符。
- `op`：`full | delta | delete`。
- `encoding`：`identity | zstd | zstd+bsdiff`。
- 同一 path 在同一清单中只能出现一次。
- `entries` 按 path 字典序写入，便于 diff 清单本身。

### 3.3 签名

- 算法：minisign（Ed25519）
- 清单签名对象：`tpk-manifest.json` 的原始字节（不转码、不重排）
- 整包另算 `sha256(tpk-file)` 写入渠道清单
- 客户端信任公钥列表写在原生 `tauri.conf.json`，支持多钥以便轮换
- 校验顺序：渠道清单签名 → 包 sha256 → 清单签名 → 路径策略 → parent 链接

### 3.4 Delta 算法（第一天写进格式）

- 算法：bsdiff 控制流 + zstd 压缩整个 patch 流
- `encoding = zstd+bsdiff`
- 应用：解 zstd → bspatch(base_bytes) → 校验结果 sha256
- 打包器策略（实现细节，不进格式）：
  - size < 262144 或不适合差分 → `full`
  - delta_blob >= 0.7 * new_size → 退回 `full`
  - 文本哈希变化几乎整文件重写时直接 `full`

---

## 4. 渠道清单（CDN 静态文件即可）

URL 由原生配置指定，例如：

`https://cdn.example.com/tpk/{channel}/latest.json`

```json
{
  "spec": "tpk-channel/1",
  "channel": "stable",
  "published_at": "2026-09-08T15:00:00Z",
  "watermark": 202609081500,
  "min_shell": "2.3.0",
  "force_shell": null,
  "notes": "修复登录页样式",
  "packs": [
    {
      "id": "core",
      "kind": "base",
      "version": "1.0.0",
      "version_code": 10000,
      "url": "https://cdn.example.com/tpk/core/base-core-1.0.0.tpk",
      "size": 8400000,
      "sha256": "hex64"
    },
    {
      "id": "core",
      "kind": "patch",
      "version": "1.0.3",
      "version_code": 10003,
      "parent_version_code": 10000,
      "url": "https://cdn.example.com/tpk/core/patch-core-1.0.3.tpk",
      "size": 180000,
      "sha256": "hex64"
    },
    {
      "id": "maps",
      "kind": "dlc",
      "version": "2.0.0",
      "version_code": 20000,
      "url": "https://cdn.example.com/tpk/maps/dlc-maps-2.0.0.tpk",
      "size": 50000000,
      "sha256": "hex64",
      "optional": true
    }
  ]
}
```

规则：

- `watermark` 单调递增；客户端持久化最大已见表；更小的清单直接丢弃（防重放）。
- `force_shell` 非空且当前壳 < 该版本：不应用任何新 pack，返回 `shell_required`。
- 同一 `id` 允许同时出现一个 base 与若干 patch；客户端选「可从本地已提交链走到的最短路径」。
- 缺失 parent 的 patch 不可单独应用。

分离签名：`latest.json.minisig`。

---

## 5. 磁盘布局（冻结）

根目录：`$APPLOCALDATA/tpk/`

```
tpk/
  state.json
  keys-cache.json          # 不存私钥；可缓存远端密钥指纹审计
  blacklist.json           # 失败包 sha256
  committed/
    layers.json
    core/
      base-core-1.0.0.tpk
      patch-core-1.0.3.tpk
    maps/
      dlc-maps-2.0.0.tpk
  booting/                 # 启动中试用的层，结构同 committed
  staged/                  # 下载完成未切换
  cache/
    tmp-*.part
  mods/                    # 未签名模组，受策略约束
```

`state.json`：

```json
{
  "spec": "tpk-state/1",
  "pointer": "committed | booting",
  "staged_rev": "optional-id",
  "booting_rev": "optional-id",
  "committed_rev": "rev-7",
  "last_watermark": 202609081500,
  "last_error": null
}
```

原子性：写 `*.tmp` → `fsync` → `rename`。Windows 上 rename 覆盖用 `REPLACE`。

安装包可把 seed base 放在 `$RESOURCE/tpk/seed/`。首次启动复制到 `committed/`（已存在则跳过）。

---

## 6. 叠加与解析（冻结语义）

层从低到高：

0. EmbeddedAssets（编译进壳，永不可删）  
1. Seed / committed base（`kind=base`，每个 id 一层）  
2. 该 id 的 patch，按 `version_code` 升序  
3. 已启用 DLC  
4. 已启用且通过策略的 Mods  

`get(path)`：

```
for layer in high_to_low:
  if layer has delete(path): return None
  if layer has full(path): return decode(blob)
  if layer has delta(path):
      base = resolve_below(path)
      if base.sha256 != delta_base_sha256: fail → treat layer corrupt
      return bspatch(base, blob)
return embedded.get(path)
```

失败层：将该包 sha256 写入 blacklist，本层跳过，不崩溃。若结果是白屏，依赖三态回滚而不是解析时 panic。

索引：启动时把所有启用包的 manifest 载入 `Arc<Index>`。`get` 热路径禁止重新解 ZIP 中央目录。

缓存：解码后的对象用内存 LRU（默认 32 MiB，可配）。delta 结果按 `(path, top_layer_hash)` 缓存。

`iter()`：返回叠加后的可见路径并集（删除的不出现）。

`csp_hashes(html)`：对**解析后的最终 HTML 字节**计算。不要使用嵌入层的旧 hash。

---

## 7. 三态状态机（冻结）

```
committed ──download ok──► staged
staged ──next cold start──► booting   # 必须在创建 WebView 前落盘
booting ──notify_ready──► committed
booting ──crash / timeoutless fail──► blacklist + rollback to previous committed
                                └── 若无 committed 内容层 → Embedded
```

规则：

- 进程内不自动切层。正在运行的 WebView 始终看启动时冻结的 `Arc<Index>`。
- `notify_ready` 可重复调用，第二次是 no-op。
- 不存在「渲染 3 秒就算成功」。
- `reset`：删除 staged/booting/committed 内容层，blacklist 保留，下次 Embedded + seed。

---

## 8. 原生配置

`tauri.conf.json`：

```json
{
  "plugins": {
    "tpk": {
      "enabled": true,
      "channel": "stable",
      "manifest_url": "https://cdn.example.com/tpk/{{channel}}/latest.json",
      "pubkeys": ["RWT..."],
      "auto_check_on_launch": true,
      "auto_download": true,
      "apply": "next_cold_start",
      "cache_budget_bytes": 33554432,
      "allow_mods": false,
      "mod_protected_globs": [
        "/index.html",
        "/assets/index-*.js"
      ]
    }
  }
}
```

模板变量只允许：`{{channel}}`、`{{arch}}`、`{{target}}`、`{{shell}}`。  
JS 不能覆盖 `manifest_url` / `pubkeys`。

---

## 9. 插件命令与事件（冻结）

权限：

```
tpk:default          = check + download + notify_ready + status
tpk:allow-reset      = reset（客服/调试）
tpk:allow-mods       = 列举/开关 mods
tpk:deny-all
```

默认只给主窗口 `tpk:default`。

### 9.1 Commands

`check() → CheckOutcome`

```ts
type CheckOutcome =
  | { status: "up_to_date"; watermark: number }
  | { status: "available"; packs: PackRef[]; bytes: number; notes?: string }
  | { status: "shell_required"; min_shell: string }
  | { status: "blacklisted"; sha256: string }
  | { status: "disabled" }
```

`download() → DownloadOutcome`

```ts
type DownloadOutcome =
  | { status: "staged"; rev: string; bytes: number }
  | { status: "already_staged"; rev: string }
  | { status: "up_to_date" }
  | { status: "shell_required"; min_shell: string }
  | { status: "failed"; code: ErrorCode; message: string }
```

`notify_ready() → { status: "committed" | "noop"; rev: string | null }`

`status() → { pointer; layers; shell; watermark; pending }`

`reset() → void`

`set_mod_enabled(id, enabled)`（需 `tpk:allow-mods`）

拒绝用 throw 表达业务分流；网络/IO 用 `failed`。编程错误才 err。

### 9.2 Events

- `tpk://download-progress` `{ downloaded, total, pack_id }`
- `tpk://state` `{ pointer, rev }`
- `tpk://error` `{ code, message }`

### 9.3 前端约定

```ts
await notifyReady();          // 壳 mount 后立刻
const c = await check();
if (c.status === "available") await download();
// 提示「下次启动生效」，不要热替换正在跑的模块图
```

---

## 10. CLI（构建与发布，一次做完）

```
tpk pack   --kind base|patch|dlc|mod
           --id core
           --version 1.0.3
           --version-code 10003
           --min-shell 2.3.0
           --dist ./dist
           --parent ./base-core-1.0.0.tpk
           --out ./out/patch-core-1.0.3.tpk
           --delta-threshold 262144

tpk sign   --key ~/.keys/tpk.key --file out/*.tpk --file channel.json

tpk inspect file.tpk
tpk diff   old.tpk new-dist/
tpk channel --packs ... --out latest.json --watermark auto

tpk verify --pubkey ... --file ...
```

退出码：0 成功；2 校验失败；3 参数错误。CI 必须跑 `verify`。

环境变量：`TPK_SIGNING_KEY`、`TPK_SIGNING_KEY_PASSWORD`。禁止把私钥放进仓库。

---

## 11. 与官方 Updater 的协议

壳版本 = `tauri.conf.json` / `package.version`，SemVer。

客户端决策表：

| 条件 | 动作 |
|---|---|
| 渠道 `force_shell` > 当前壳 | 只提示/调用官方 updater，忽略 pack |
| pack.`min_shell` > 当前壳 | 跳过该 pack |
| 仅内容变化 | 只走 TPK |
| Rust 命令签名变化 | 升壳，同时可带新 seed base |

禁止 pack 内携带：

- 可执行文件、dylib/dll/so
- sidecar
- 改写 `$RESOURCE` 下的壳文件

下载器用插件内 `reqwest`/`ureq`，不要把 CDN 文件暴露给前端 `fetch` 后写入任意路径。

---

## 12. 安全模型（终态）

信任根：嵌入壳内的 pubkey 列表。

威胁与对策：

| 威胁 | 对策 |
|---|---|
| 篡改 CDN | minisign + sha256 |
| 重放旧清单 | watermark |
| XSS 改更新源 | URL/公钥仅原生配置 |
| 半写入包 | tmp+rename；启动校验 |
| 坏包白屏 | booting 未 ack → 拉黑回滚 |
| Zip slip | 规范化路径，拒绝 `..` |
| 模组覆盖登录页 | `mod_protected_globs` |
| 降级攻击 | version_code 单调；拒绝更小 code 覆盖同 id |
| 密钥泄漏 | 多公钥；轮换时新旧共存一个发布周期 |

Mods：`trusted=false`，默认不能覆盖受保护 glob；即使 `allow_mods=true` 也不走 CDN 自动装未签名包。

---

## 13. 运行时接入（代码契约）

```rust
pub fn run() {
    let mut ctx = tauri::generate_context!();
    let tpk = tauri_plugin_tpk::attach(&mut ctx);
    tauri::Builder::default()
        .plugin(tauri_plugin_tpk::init(tpk))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .run(ctx)
        .unwrap();
}
```

`attach`：

1. 取出当前 `EmbeddedAssets`
2. 构造 `PackAssets { fallback, store: lazy }`
3. `ctx.set_assets(Box::new(pack_assets))`

`init` 的 `setup`：

1. 解析 `$APPLOCALDATA/tpk`
2. 执行状态机晋升
3. 重建 Index 并替换 PackAssets 内部 `RwLock<Arc<Index>>`
4. 此时才允许创建窗口

开发模式：存在 `devUrl` 时 WebView 仍打 Vite；`PackAssets` 继续工作供 `AssetResolver` 与 Rust 读包。可用 `TPK_DEV_LAYERS` 指向本地 tpk。

---

## 14. 依赖（商业友好）

| 用途 | Crate | 许可 |
|---|---|---|
| 序列化 | serde / serde_json | MIT OR Apache-2.0 |
| 哈希 | sha2 | MIT OR Apache-2.0 |
| ZIP | zip | MIT |
| 签名 | minisign 兼容实现或 ed25519-dalek + 自写验签 | MIT OR Apache-2.0 |
| 差分 | bsdiff 0.2 | BSD-2-Clause |
| 压缩 | 优先纯 Rust zstd 解码；编码在 CLI 可用官方 zstd | 运行时避免把 GPL 选择带进 app |
| HTTP | reqwest rustls | MIT OR Apache-2.0 |
| SemVer | semver | MIT OR Apache-2.0 |

`cargo deny` 默认拒绝 GPL/AGPL 进入 `tauri-plugin-tpk` 运行时依赖。

---

## 15. 错误码（冻结）

```
E_DISABLED
E_NETWORK
E_SIGNATURE
E_HASH
E_SPEC
E_PATH
E_PARENT
E_SHELL
E_WATERMARK
E_BLACKLIST
E_IO
E_DELTA
E_STATE
E_POLICY
```

日志不打印完整 URL query 中的令牌。

---

## 16. 测试矩阵（必须与实现一起交）

格式：

- 规范化路径拒绝用例
- 重复 path / 未知 op / 坏 spec
- tombstone 遮蔽下层 full
- 高层 full 覆盖下层 delete
- delta 基哈希不匹配
- 签名错误 / 截断 ZIP

状态机：

- staged 后杀进程，再启动进入 booting
- booting 不 ack 再启动回滚并拉黑
- notify_ready 后杀进程，层保持 committed

客户端：

- watermark 回退被拒
- min_shell 拦截
- 只缺 patch 时不重下 base
- 磁盘满时 staged 不切换指针

集成：

- 替换 index.html 后 origin 仍是 tauri://，localStorage 仍在
- CSP hash 随最终 HTML 变化
- 官方 updater 与 TPK 同时配置不抢 Assets

---

## 17. CI / 发布流水线

```
frontend build
  → tpk pack (base if new major content tree else patch vs last released parent)
  → tpk sign
  → tpk channel
  → tpk verify
  → upload CDN
  → 若有 Rust 变更：并行 tauri build + 官方 updater latest.json
```

渠道合并：三平台壳更新各打 latest.json；**内容 pack 与平台无关**，只发一份。不要把 tpk 塞进 per-arch updater json。

版本号纪律：

- 壳：`x.y.z`（Cargo + tauri.conf + package.json 同步）
- 内容：每 `id` 独立 `version_code`
- 渠道 `watermark`：UTC `YYYYMMDDHHMM` 或构建号，只增不减

---

## 18. 性能预算

| 项 | 目标 |
|---|---|
| 冷启动额外开销（已提交 3 层） | ≤ 30 ms 索引加载（SSD） |
| `get` 已缓存 | ≤ 0.1 ms |
| `get` 未缓存小文件 | ≤ 2 ms |
| 首包 seed 复制 | 仅首次安装 |
| 下载 | 断点续传 `.part`，校验后再 rename |

超过 20000 条目时清单仍应可接受；索引用 `HashMap<Box<str>, Loc>`。

---

## 19. 实施顺序（契约不变，只排工期）

工期按「写最终 API」，禁止出过渡格式。

1. `tpk-format` + schema + `tpk inspect/verify`  
2. `tpk pack`（含 delta 与 delete）+ `tpk sign` + `tpk channel`  
3. `tpk-resolve` + 单测叠加矩阵  
4. `tpk-store` 三态  
5. `tauri-plugin-tpk` attach/init/commands  
6. `tpk-client` 下载与渠道  
7. example-shell + CI  
8. cargo deny + 模糊测试路径解析  

每一步都产出最终文件格式，不允许「先 zip 再升级成 tpk」。

---

## 20. 应用接入清单（完成定义）

1. 二进制嵌入保底 `frontendDist`  
2. 可选 `$RESOURCE/tpk/seed/base-*.tpk`  
3. `attach` + 插件最先注册  
4. capability：`tpk:default`  
5. 前端启动调用 `notifyReady`  
6. CDN 放置 `latest.json` + `.minisig` + packs  
7. 公钥写入 `plugins.tpk.pubkeys`  
8. Rust 变更走官方 updater，内容走 tpk  

至此，系统在产品意义上一次到位：协议、安全、回滚、增量、DLC、与壳更新的边界均已闭合。

---

# 附录 A：对本规范的已确认修订（实现以本附录为准）

本附录记录规范正文与 Tauri v2 实际 API、两大应用商店政策、以及实测性能数据之间的冲突，
以及为解决这些冲突而对**冻结契约**所做的改动。正文与附录冲突时，**以附录为准**。

修订依据的核实日期：**2026-09-11**（Tauri 2.11.3 / tauri-utils 2.9.3 / tauri-codegen 2.6.3；
Apple App Review Guidelines `Last Updated: June 8, 2026`）。

## A.1 与 Tauri 实际 API 的冲突

| 编号 | 正文 | 实际 | 处置 |
|---|---|---|---|
| F1 | §13「替换 PackAssets 内部 `RwLock<Arc<Index>>`」 | `CspHash<'a>` 持有借用（`tauri-utils/src/assets.rs:88-95`）。`RwLock` 方案技术上可写（先 collect 再 drop guard），但 §7 已规定「进程内不自动切层」，运行时换层能力是多余的 | 用 `OnceLock<Arc<Index>>`，一次装入不可变 |
| F3 | §13「此时才允许创建窗口」 | 插件 `setup` 在 `Builder::build()` 末尾（`app.rs:2440`），窗口创建在 `App::run()→setup()`（`app.rs:2521`），`Assets::setup` 更在窗口之后（`app.rs:2528`） | 状态机晋升与索引构建放插件 `setup`；**`setup` 永不返回 `Err`**——返回 Err 会让 `Builder::build` 失败、app 起不来，磁盘损坏必须降级到 embedded 而非启动崩溃 |
| F4 | §6「`csp_hashes(html)` 对解析后的最终 HTML 字节计算」 | 编译期 CSP global hash 是对 `.js`/`.mjs` **文件内容**算 sha256（`tauri-codegen/src/embedded_assets.rs:176-198`）；内联脚本走 `inline_scripts[html_key]`。且 `manager/mod.rs:148-152` 只要有 hash 就无条件注入 `'self'`，同源外链 js 本就放行 | 自算 global hash 是装饰性的。真正的问题是 OTA 替换 HTML 后仍注入**编译期旧 HTML 的内联脚本 hash**。**`tpk pack` 硬拒含非空 `<script>` 的 HTML**；`csp_hashes` 仅在 embedded HTML 实际被服务时返回 fallback hash |
| F7 | §5 `$APPLOCALDATA/tpk/` | Android 的 `app_local_data_dir()` 走 JNI（`path/android.rs:144`），attach 阶段不可用；iOS 无 `path/ios.rs`，走 desktop 实现 | 路径解析、seed 复制、状态机晋升全部延后到插件 `setup`，用 `app.path()` |
| F9 | — | `tauri://` 协议**无 Range/206 支持**（`protocol/tauri.rs:176-186`）；有 Range 的 `asset://` 完全绕开 `Assets` trait | TPK 不是媒体分发通道。包内音视频无法 seek；前端用 `convertFileSrc` 看不到叠加层 |
| F11 | — | isolation 模式硬编码 `assets.get("index.html")` 并依赖编译期注入的 hook（`protocol/isolation.rs:40`） | OTA 与 `Pattern::Isolation` 很可能不兼容；`attach()` 检测到即告警 |
| F12 | — | mime 推断用**原始请求 path** 而非命中的 asset path，且扩展名表很短，未知扩展名兜底 `text/html`（`tauri-utils/src/mime_type.rs:56-76`） | `tpk pack` 强制保留文件扩展名，否则 `.wasm`/`.woff2` 会拿到 `text/html` |

## A.2 冻结格式的改动

### M1 — 磁盘布局改为内容寻址的单一层池（替换 §5 的三平行目录）

`committed/` `booting/` `staged/` 三个平行目录意味着晋升＝多文件 rename。中途被杀会留下
「`state.json` 说 committed（可信状态，无 ack 循环兜底）而层集合残缺」→ **永久白屏，只能重装**，
直接违反 §0「坏包永不砖机」。

```
$APPLOCALDATA/tpk/
  state.json                  # {pointer, staged/booting/committed 各持一份 sha 列表, ...}
  blacklist.json
  keys-cache.json
  layers/<file_sha256>.tpk
$APPCACHE/tpk/                # 见 M8
  materialized/<result_sha256>
  tmp-*.part
```

晋升＝一次 `state.json` 原子写；`gc()`＝引用计数扫描，幂等。同一个 base 被多个 rev 引用只存一份。

### M2 — `entries[]` 增加 `blob_sha256` 与 `blob_size`

`op=delta` 的 blob 名是 delta 自身的 sha256，但清单里**没有任何签名覆盖的字段**是该 blob 的哈希
（`sha256` 是结果哈希，`delta_base_sha256` 是基础哈希）。同时 zstd 解压无上界：100KB 的 `.zst`
可以解成 10GB 的 patch 流，**在 bspatch 开始前就 OOM**。

每个 `op != delete` 的 entry 必须带 `blob_sha256`（blob 原始字节）与 `blob_size`（压缩后字节数）。
解码前校验 `blob_sha256`；zstd 解码用 `.take(blob_size)` 封顶。

### M3 — `state.json` 增加 `boot_attempts` / `consecutive_rollbacks` / `install_id`，`last_watermark` 改 per-channel map

- §7 的二值语义把「坏包」与「用户退出 / OS 内存压力杀进程 / 掉电」混为一谈。移动端上后者是日常事件。
  `boot_attempts < 3` 时重试 booting，不回滚不拉黑；`notify_ready` 成功清零。
- `consecutive_rollbacks >= 3` 时强制关闭 `auto_check_on_launch` / `auto_download`，
  `status()` 返回 `degraded: true`，只能通过 `reset` 恢复。
- `last_watermark` 是单标量时，stable→beta→stable 会让所有 stable 清单因 watermark 更小被永久丢弃，
  **更新静默死亡**。改为 `{"stable": N, "beta": M}`。
- `install_id`（UUIDv4，`reset` 不清除）用于灰度分桶，见 M9。

`notify_ready` 必须在返回给 JS **之前**完成 `state.json` 的 fsync+rename，不得 spawn 异步任务。

### M4 — blacklist 条目结构化，按 `(id, version_code)` 或 sha256 匹配，分级 TTL

按 sha256 存意味着重新打包同样的内容（mtime/压缩级别/entry 顺序变化）就换了 sha256，
**「重跑一次 CI」即可绕过**。且无 TTL、无清除、`reset()` 保留 ⇒ 误杀即永久拒绝服务。

条目：`{sha256, id, version_code, reason: ErrorCode, count, first_seen}`。
分级：`E_SIGNATURE`/`E_SPEC`/`E_PATH`/`E_PARENT` 永久；`E_IO`/`E_HASH`/`E_DELTA`/`E_STATE`
三振 + 30 天 TTL。上限 256 条 FIFO。`reset({clear_blacklist: true})` 可清。
必须在 **plan / stage / boot 三处**都查。

配套：**`tpk pack` 强制确定性输出**（固定 entry 顺序、mtime=0、剥 extra field、固定压缩参数），
否则 blacklist 失效且渠道清单 sha256 不可复现。

### M5 — delta 物化移出渲染热路径，且**不得在 `boot()` 同步执行**

原始问题：`Resolver::get` 里做 zstd 解码 + 递归物化 base + bspatch，而 WebView 每次资源请求都走 `get`。

**但把物化搬进 `boot()` 会造成更严重的问题**：物化发生在窗口创建之前（F3），
iOS 启动 watchdog **20 秒 wall clock 硬杀**（`0x8badf00d`），Android input dispatch ANR 5 秒。
一个含 100 个 delta entry 的层在中端 Android 上需要 8–15 秒，老 iPhone 上 10–20 秒。
更糟的是它与 M3 的 `boot_attempts` 三振串联：**一个内容完全正确的包，只因为在慢设备上物化超时被系统杀三次，
最终被永久拉黑**——正是 M3 想修的误杀，M5 又造了一个新入口。

最终语义：
- **`Store::stage()` 时物化**（下载完成后的异步任务里，可挂进度 UI），不在 `boot()`。
- `boot()` 只做存在性检查。缺失的 entry 标 `Pending`，由后台线程物化，`Resolver::get` 只阻塞那一个 entry。
  （`materialized/` 在 `$APPCACHE` 下会被系统清空，所以懒物化路径**无论如何都必须存在**。）
- `TpkConfig.max_boot_blocking_ms`：桌面默认 150，**移动端默认 0**。`boot()` 内同步磁盘工作累计超过即转后台。
- `tpk pack` 侧上限：delta entry 数 ≤ 64 **且** delta 结果总字节 ≤ 32 MiB，超出自动退回 `full`。

### M6 — 密钥轮换需要 `key_epoch`（轮出机制）

§12 的「多公钥；轮换时新旧共存一个发布周期」只描述了轮入。`pubkeys` 编译进壳，
要下掉泄漏的 K1 必须发新壳 → 走官方 updater → 渗透数周到数月，
而这个窗口恰好覆盖「不升壳」这个 TPK 的核心人群。

- `pubkeys` 改为 `[{"key": "RWT...", "epoch": 1}]`
- 渠道清单增加**签名覆盖的** `key_epoch: N`
- 客户端持久化单调 `min_key_epoch`（`keys-cache.json`），**拒绝 `epoch < min_key_epoch` 的密钥**

持 K1 的攻击者只能把 epoch 往高推（需要 K2 才能签 epoch=2），推高只会加速自己出局，且无法回退。
（「清单里放 revoked 列表」不可行——那是可被泄漏密钥反向用来 DoS 掉好密钥的武器。）

**SLA**：密钥撤销的实际生效时间 = 壳更新渗透时间 + key_epoch 传播时间。桌面 4–8 周，移动端可能 3 个月以上。

### M7 — 不得静默丢失的字段

`policies.{can_override, cannot_override, trusted}`（§3.2）、`max_shell`（§3.2）、
渠道条目的 `optional`（§4）必须被解析。本轮不实现的策略字段**强制必须是默认值**
（`can_override=["**"]` / `cannot_override=[]`），非默认值直接 `E_SPEC` ——
否则将来启用时会遇到「历史包里有意义不明的策略」。`max_shell` 必须在 `plan` 里生效。

`keys-cache.json` 落地为 M6 的 `min_key_epoch` 存储。`layers.json` 因 M1 不再需要。

### M8 — 磁盘布局拆成两根（Application Support + Caches）

iOS 上 `$APPLOCALDATA` = `Library/Application Support`，**默认参与 iCloud 备份**。
App Review Guidelines 的提审 checklist 逐字把 *Optimizing Your App's Data for iCloud Backup*
列为审核依据文档（对应旧 Guideline 2.23，有大量公开拒审记录）。
Android 侧 Auto Backup 有 25MB 上限，**超了会静默跳过整个 app 的备份**。

- `materialized/` 与 `tmp-*.part` → `$APPCACHE/tpk/`（可重算/可重下，系统清空无妨）
- `layers/*.tpk` 留在 Application Support，但设 `isExcludedFromBackup = true`
  （`#[cfg(target_vendor = "apple")]`，约 15 行 objc2；失败只 log 不 fail）
- Android 提供 `backup_rules.xml` 片段 exclude `tpk/layers`

### M9 — 渠道清单增加 `rollout`，客户端按 `install_id` 自分桶

§4 只有 channel 没有百分比灰度。`plugins.tpk.channel` 在原生配置里且 JS 不能覆盖，
所以「多 channel 分桶」做不到；CDN edge 逻辑会摧毁「纯静态托管」的定位。

- `packs[]` 增加可选 `"rollout": 1..100`，**缺省 100**（老清单天然兼容）
- 判定：`sha256(install_id ‖ ":" ‖ pack.id ‖ ":" ‖ pack.version_code)[0..8] % 100 < rollout`
  （hash 含 `version_code` 使每个包重新洗牌，不含 `channel` 使切渠道不重新洗牌）
- **不在桶内时照常推进 watermark**，否则被排除的设备会卡在旧 watermark，切回全量时反被 M3 的下限咬住

### M10 — watermark 比较语义：拒绝严格更小，接受相等

§4 只写了「更小的清单直接丢弃」。若实现为严格 `>`，一次同分钟误发就让那份清单
对所有见过前一份的设备**永久静默失效**——而静默失效是最难排查的故障类别。
真正的防降级防线是 pack 级的 `version_code` 单调检查，不是 watermark。

配套：**渠道清单验签失败只重试，不写 blacklist**（blacklist 是包级机制，
把 CDN 的瞬时不一致算成 `E_SIGNATURE` 会永久禁掉一个好包）。

## A.3 §18 性能预算重写

正文的四个数字有两个是过度工程，而唯一用户能直接感知、且有硬性系统约束的量没有写。

| 原条目 | 处置 |
|---|---|
| 冷启动额外开销 ≤ 30 ms 索引加载 | **保留但改写**：`≤ 30 ms（≤2000 entry/层，≤3 层）；且索引加载耗时必须与资源总字节数无关（O(entry) 不得 O(bytes)）`。后半句才是能抓住 bug 的那句——它同时否决了「boot 时全量校验层 sha256」和「boot 时物化 delta」两个设计 |
| `get` 已缓存 ≤ 0.1 ms | **删除**。一次导航 10–50 个子资源，0.1ms vs 1ms 的差在 layout/paint 的噪声里；且首屏时缓存命中率约等于 0。**替换为结构约束**：「缓存命中路径不得产生多于一次的字节拷贝」 |
| `get` 未缓存小文件 ≤ 2 ms | **改粒度**：`首屏全部子资源解析总耗时 ≤ 100 ms，且并发请求不得完全串行化`。单次 2ms 无意义，50 个请求串在一把锁上才是用户看得见的 |
| 超过 20000 条目仍可接受 | **改为内存预算**：`索引常驻内存 ≤ 200 B/entry/层` |
| （缺失） | **新增**：`插件 setup 内同步阻塞总时长 ≤ 200 ms（桌面）/ ≤ 100 ms（移动）`。这是唯一有硬性系统约束（iOS 20s watchdog、Android 5s ANR）的量 |

实测基线（Apple Silicon）：sha256 2.3 GB/s；Ed25519 verify 87 µs/次。
由此：**层文件 sha256 全量校验不是「免费」的**（3 层 × 20MB = 26–60ms 桌面 / 60–200ms 移动，
单项即吃光预算且随内容体积线性增长）。层完整性只在 `stage()` 时算一次并记录
`(size, mtime, inode)`，`boot()` 只比对元数据。运行时的真防线是 M2 的 per-blob 懒校验 + 清单签名。

热路径的数量级开销是 **ruzstd 解码**（200KB 约 0.5–1.0 ms 桌面 / 1.3–2.5 ms 移动），
比 sha256 后置校验（0.09 ms）大一个数量级。**后置 sha256 校验必须保留**，
它只占预算 5–10%，且是唯一覆盖「blob 落盘后被改写」的检查。

**不保留 `ZipArchive`**：索引构建期抄出 blob 的 `(data_start, compressed_size, method)` 后
drop 掉 `ZipArchive`，只留 `File`，读取用 `read_at`/`seek_read`（只需 `&File`）。
省约 400 B/entry 的常驻内存（20000 entry × 3 层 ≈ 24 MB），且**无需任何锁**。

移动端默认值：`max_asset_bytes` 16 MiB（桌面 64 MiB）、`cache_budget_bytes` 8 MiB（桌面 32 MiB）、
`max_delta_chain_depth` 2（原 8 ⇒ 峰值 160MB+，在 2–3GB 设备上是 jetsam/LMK 击杀风险）。
bspatch 输出用 `Vec::with_capacity(entry.size)` 精确预分配（`entry.size` 被签名覆盖且已在 parse 阶段校验上限）。

## A.4 应用商店合规（§11 的补充）

**Guidelines 2.5.2 当前原文里没有 interpreted code / WebView 豁免**，唯一豁免是教学编程类 app。
真正的豁免在 DPLA §3.3.1(B)（合同，不是审核指南），且附三个条件：
不改变 primary purpose、不绕过 OS 安全特性、不构成 storefront。
Google Play 的豁免是成文的：「does not apply to code that runs in … an interpreter
(such as JavaScript in a webview or browser)」。

由此产生的硬性约束：

| 约束 | 依据 |
|---|---|
| `PackKind::{Dlc, Mod}` 在 App Store target 下**编译期不存在**（`#[cfg]` gate） | DPLA §3.3.1(C)「may not provide, unlock or **enable** additional features … through distribution mechanisms other than the App Store」（**与付费无关**）；Guidelines 3.1.1 逐字列举 game levels；IAP Attachment §2.4「购买后只能下 data 不能下 code」；3.2.2(i) 禁止第三方插件界面 |
| 移动端 `auto_check_on_launch` / `auto_download` 默认 `false`（§8 现为 `true`） | Guidelines 2.3.1(a)「hidden, dormant, or undocumented features」；默认静默 OTA 即默认 dormant feature |
| 移动端不注册 `tauri-plugin-updater`（§13 示例里与 tpk 并列） | 官方 updater 做二进制替换，两商店均明确违规 |
| `CheckOutcome::ShellRequired` **禁止携带 url 字段**，插件不自带任何 UI | Play「may not modify, replace, or update itself using any method other than Google Play's update mechanism」 |
| `notes` 截断 200 字符 + 剥 HTML，文档标注「仅供日志」 | 否则是一个完全由 CDN 控制的未审核文案通道 |
| `tpk pack` 拒绝任何 `src`/`href` 指向 `http(s)://` 的 `script`/`iframe`/`worker` | Play DNA 例示「webview with added JavaScript Interface that loads untrusted web content」；同时解决 F4 的 CSP 问题 |
| 必须在 Notes for Review 中具体描述 OTA 机制并使其 accessible for review | Guidelines 2.3.1(a) |

**反直觉但重要**：iOS 沙盒不允许 fork/exec 且无 sidecar，所以 DPLA (b) 款
（绕过 OS 安全特性）的最坏情况在 iOS 上**显著小于桌面**。
真正的 (b) 款重灾区是 macOS App Store 与 Microsoft Store。

## A.5 §17 发布流水线的补充

- `version_code` 推荐 UTC `YYYYMMDDHHMMSS`。§17 的「或构建号」应删除：
  `github.run_number` 在迁仓库/重建 workflow/换文件名时会**重置**，一次重置就让所有客户端的单调检查永久拒绝新内容。
- 每次发布必须同时产出不可变副本 `{channel}/{watermark}.json`（回滚与审计用）。
- **内容回滚不存在「退回旧版本」**：客户端的 `version_code` 单调检查会拒绝更低的 code，
  且下限还取「层池里实际 version_code 的 max」。正确做法是**把旧内容用更高的 version_code 重新发布，
  且发 base 而非 patch**（此刻线上同时存在两个 cohort，patch 的 parent 只能指向其中一个）。
- 内容回滚的真实 RTO = 用户下一次冷启动（§7「进程内不自动切层」），桌面端可能是几小时到几天；
  移动端因 `auto_check_on_launch` 默认 false 还要加上用户主动触发的时间。
- `min_shell` 的单一真相源是 `tauri.conf.json` 的 `plugins.tpk.min_shell`；
  发布前必须断言它 ≤ 官方 updater 已发布的壳版本，否则会**静默不更新**（`plan` 跳过该 pack，`check()` 返回 `up_to_date`）。
- `force_shell` 是大锤：它让内容通道对该壳彻底停摆，包括本可以用 TPK 下发的热修复。
  判据：**当你希望这批用户连热修复都不要拿到时，才用它。**
