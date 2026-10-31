# TPK 可靠性与安全边界优化计划

## Goal

修复审查发现的持久化、容器解析、网络输入、频道绑定和并发更新问题，使 OTA 修订在故障、重试与冷启动时保持一致。保持 MSRV 1.88、单向 crate 依赖、进程内冻结 resolver 和现有错误代码契约。

用户已授权实施全部五个阶段。按本计划修复产品代码并验证；此前核心 crate 测试通过不代表已覆盖下列故障。

## Current Phase

桌面实际验收完成。阶段7构建准备完成；阶段8新版独立诊断宿主iOS27.1的base/patch冷启动提交、delta、空404、坏包回滚/黑名单、缓存重建、reset及保留下限重装通过。正式Rust1.88示例的iOS27宿主生命周期兼容仍阻塞；专用模拟器/HTTPS/Cua桥已清理，详见progress.md Session16。

## Next Step

正式iOS27支持需独立选择宿主方案：仅示例使用Rust1.90/Tauri2.12，或评估旧工具链兼容的回补/旧SDK；插件MSRV继续1.88。新版诊断宿主的OTA流程（含模拟器reset/cache恢复）已通过，不替代正式示例兼容修复、移动实机、Android或真实掉电验收。

## Scope

- 纳入审查发现 F01–F13，先修复可复现的正确性问题。
- 不进行无关重构、不引入新 crate、不提高 MSRV、不提交或推送 Git。
- 修改冻结规范时明确行为变化；新增 docs 页面必须同步网站链接和导航。
- 估计单人实施 7–11 个开发日，包含测试、评审和桌面工作流验证；移动实机排期另计。

## Phases

### Phase 1: 保证持久化失败可重试，保留运行修订资源

Status: complete

关联：F01、F02、F10、F13。预计 2–3 个开发日。

工作：
- 先增加 commit 写入失败回归测试；构造候选状态，持久化成功后发布到内存。核对 stage/reset 的同类失败路径，建立统一状态发布规则。
- 区分“状态已耐久提交”和“后续清理失败”；GC 失败不能撤销已成功提交。
- 为当前进程 resolver 引用增加明确的保留机制。reset 清除持久修订后，继续保留当前 resolver 所需层与 delta 结果，冷启动后回收；不更换 OnceLock resolver。
- 明确各平台目录 fsync 策略；支持的平台必须传播真正的同步失败。
- 临时文件采用独占创建，碰撞换名重试，避免 File::create 跟随既有符号链接。

验收：
- boot_attempts=2 时首次 ready 写入失败，恢复后重试能真正提交；重开 Store 不回滚、不拉黑该版本。
- state 发布前失败时内存仍对应最后一个有效状态；若状态已落盘但附属操作失败，返回值和重试行为准确反映持久结果。
- 运行 delta 修订时 reset 后，未缓存和已淘汰资源仍能读取本次修订；下次冷启动才切回 embedded/seed，旧资源最终可回收。
- symlink 或既有临时路径不能截断其他文件；目录同步错误按平台策略可观察。

### Phase 2: 在解析和验签之前限制输入

Status: complete

关联：F03、F05、F06、F09。预计 2–3 个开发日。

工作：
- 先确定 zip 依赖是否提供原始中央目录条目接口；若没有，比较更换兼容依赖与实现最小有界中央目录预检查，选择能覆盖 ZIP64 且维护成本最低的方案。
- 在 ZipArchive 去重和无界分配前检查原始重复名、条目数及中央目录字节预算。避免只修改当前 HashSet 循环。
- 清单和分离签名改为流式读取，按实际累计字节限制；签名预算单独确定并记录。
- 集中创建运行时 HTTP 客户端，HTTPS-only，配置连接/读取超时，并确保重定向不能降级至 HTTP。大型包下载避免过短的固定总时限。
- 统一网络错误脱敏，去掉 reqwest 错误附带的原始 URL，再附加安全请求标识。

验收：
- 两个同名 manifest、同名 blob、65,537 个同名条目均在结构检查阶段拒绝；有效 ZIP64 和既有 golden 包继续兼容。
- 无 Content-Length 的超限清单/签名在读取越界时立即终止，而不是完整接收后拒绝。
- 网络故障的事件、日志、last_error 不包含测试用查询 token；停滞请求有明确超时；HTTPS→HTTP 重定向拒绝。

### Phase 3: 绑定频道，完善防降级规则

Status: complete

关联：F04、F08。预计 1 个开发日。

工作：
- 在任何水位线、密钥下限持久化和 plan 之前，严格检查签名清单频道等于配置频道。
- 尽量保持现有公开 API 兼容；决定在插件调用边界补校验，还是新增带 expected_channel 的验证接口，避免默默改变既有公开函数签名。
- 修订 spec 对 patch/version_floor 的规则：最终准备激活的每个 pack id 版本不得低于已提交下限。
- 明确合法多 patch 链中低版本中间依赖的处理：可以为构造最终合规版本保留必要依赖，但不得独立激活低于 floor 的最终修订。对 stage/seed 路径检查同一规则的适用范围。
- 保持启动失败回滚策略与“通过更新重新安装旧内容”的限制分开，不阻断已承诺的安全回滚。

验收：
- 同一信任钥下 stable 请求收到 beta 清单时拒绝，且配置频道 watermark/min_key_epoch 不变。
- installed=1、floor=3、只提供 parent=1 的 patch2 时不能激活；合法恢复至 >=3 的链仍可执行。
- reset/seed、condemn 后规划、正常回滚都有明确测试，规范、错误映射和文档一致。

### Phase 4: 协调并发更新，恢复可继续的下载与独立 delta

Status: complete

关联：F07、F11、F12。预计 1–2 个开发日。

工作：
- 自动和手动下载共用异步操作协调机制，避免同时写相同 part/final 文件；锁不得跨 await 持有同步 MutexGuard。
- reset 与在途下载协调：明确采用等待完成后 reset，或取消并等待清理后 reset；保证 reset 返回后旧计划不能重新 stage。
- 固定单次下载使用的计划快照；成功 stage 后正确处理 pending，避免重复消费。暂不默认新增前端 plan token，除非现有签名不能满足行为要求。
- 验证 206 Content-Range 的起点、总长度和响应长度；合法但未完成的短响应保留有效前缀，哈希错误清除损坏结果；保持忽略 Range 返回 200 时重下规则。
- 缓存恢复逐层收集失败，继续恢复依赖完整的独立 id；保留 stage 的严格全成功语义，区分启动恢复与事务性 stage。

验收：
- 自动更新与两次手动下载重叠不互删文件、不重复 stage；事件和最终状态一致。
- reset 与下载竞争时结果确定，reset 返回后不能被此前下载重新填充。
- 正确/错误的 206、200、短响应及中断重试均通过本地 HTTP fixture 验证。
- 前一层 delta 失败不阻止后续独立 id 的恢复；依赖损坏内容的层不会被错误恢复。

### Phase 5: 验证完整工作流，交付可评审改动

Status: complete

完成范围：自动化测试、编译/静态检查、规范与文档同步及审查记录。未执行原生GUI、移动实机或真实掉电测试，不把mock/状态机测试当作这些验收通过。

关联：全部修复。预计 1–2 个开发日，可与前面阶段部分重叠。

工作：
- 每个阶段先跑相关回归和 crate 测试，保留小范围可审查 diff；最后统一运行下列检查，避免反复执行完整构建。
- 更新 spec、已有 docs 和需要调整的 JSON schema；若新增 docs 页面，同步网站 symlink、sidebar、相关索引。
- 汇总行为变化、故障注入结果、兼容性和未完成的实机验证；用户自行处理 Git 提交/推送。

最终检查：
```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo +1.88 check --workspace --all-features
cargo check --manifest-path examples/shell-app/src-tauri/Cargo.toml
cargo check --workspace --all-targets --features app-store,test-packs,test-keys
cargo clippy --workspace --all-targets --features app-store,test-packs,test-keys -- -D warnings
cargo test --workspace --features app-store,test-packs,test-keys
pnpm -C packages/tauri-plugin-tpk-api install --frozen-lockfile
pnpm -C packages/tauri-plugin-tpk-api build
node --test scripts/content-release.test.mjs
```

桌面验收：通过真实冷启动执行下载→stage→booting→ready→committed，加入持久化故障与 reset 场景。移动 OTA 必须使用 ios/android build --debug，不能用代理 dev server 的模式代替 Assets 验证；设备不可用时明确记录未验收。

### Phase 6: 真实桌面冷启动验收

Status: complete

用户已请求“继续实施”，本阶段延续原计划尚未完成的设备验证。准备已完成：smoke.mjs生成并验证base/patch/broken，6项测试通过（含回滚可观测性），独立macOS.app已构建。未把这些结果计为原生GUI验收。

独立app：com.example.tpk.smoke.s14ebc6960523；用户已授权并完成前台验收。实际base/patch/reset/empty404/cache重建/三次未ready回滚及黑名单通过；CA信任已撤销，服务和全部测试实例退出，原前台恢复。证据和范围见progress.md。

墓碑404修复选择：增加可组合响应guard与宿主builder接入，保留Tauri内置协议/SPA/dev代理/宿主回调；不采用全局tauri协议覆写。

- 检查桌面 GUI 权限、Xcode/模拟器/Android 工具链可用性，不把缺失工具当作通过。
- 准备独立的本地签名包和 HTTPS 验收流程；生成测试密钥不得写入可提交/共享文件，保持生产 TLS 校验。
- 在可用环境中验证真实窗口加载、下载→stage→冷启动→ready/committed，及坏包多次冷启动回滚。
- 对 reset 运行资源保留、缓存清除后的 delta 恢复准备明确验收步骤与结果记录。
- 设备或操作权限不可用时，完成可交付的脚本/文档和桌面构建后列明阻塞，不用 mock 结果替代真实验收。

### Phase 7: 隔离iOS模拟器构建准备

Status: complete

已完成：ios-simulator.sh入口、Swift native局部包装、持久TMPDIR、Rust1.88模拟器--locked检查；授权安装原生依赖与runtime后init/build成功，完整.app为arm64 IOSSIMULATOR/sdk27.1/minos15.0。未启动模拟器或进行移动OTA运行，产物仍为默认示例配置。

- 在target/ios-rustup安装独立Rust1.88.0 minimal+aarch64-apple-ios-sim，不更改全局默认工具链或~/.rustup。
- 生成ignored gen/apple项目，跳过CLI自动全局target安装；build --debug --target aarch64-sim --no-sign。
- 不打开Xcode、不启动模拟器、不修改开发者账号/签名/证书信任。需要外部账号或额外全局依赖时记录具体阻塞。
- 编译成功仅作为simulator artifact证据，不能视为移动真实运行或实体设备验收。

### Phase 8: iOS模拟器真实OTA运行

Status: partial — 新版诊断宿主核心OTA通过；正式Rust1.88示例兼容阻塞

- 已获完整运行/临时模拟器信任/前台与清理授权，专用签名会话和.app构建完成。
- iOS27.1实际安装启动后UIKit在NoSceneLifecycleAdoption处SIGTRAP，调用栈经tao/Tauri；尚未触发OTA操作。iOS26.5替代设备首次系统迁移未完成，不计运行通过。
- 两个专用设备已删除，测试CA随设备移除；HTTPS已停，Device Hub窗口已退出，未改Mac信任或用户原有设备。
- 上游调查与隔离诊断已完成：Rust1.90/Tauri2.12.1/runtime-wry2.12.3/tao0.37.2+静态scene配置越过原trap。Loading根因是诊断配置漏掉withGlobalTauri；补回后实际IPC、embedded→base1→patch2冷启动提交、delta 200 PATCH DATA、墓碑空404通过。
- 坏包3两次booting未ready，第三次恢复committed rev-2；黑名单not_acknowledged/count3，floor2保留，再check为up_to_date。证据见progress.md Session16。
- 正式项目MSRV1.88未变，不直接升级正式依赖。专用设备及其中CA已删除，HTTPS与Cua桥已停，Device Hub窗口已退出。模拟器reset/cache恢复及保留下限重装已在Session17补齐；移动实机与Android未验收。

## Decisions Made

| 决策 | 理由 |
| --- | --- |
| 先处理 F01/F02，再处理输入边界 | 直接影响已运行修订的正确提交和资源一致性 |
| 保持 resolver 进程内冻结，通过引用保留修复 reset | 遵守现有架构，避免混用模块 |
| ZIP 在去重前检查 | 当前 archive.len/by_index_raw 已丢失原始重复信息 |
| patch floor 同时修改规范与实现 | 现有冻结规范确实只要求非 patch 检查，不能只称为实现不符合规范 |
| 先保留现有前端命令签名 | 并发协调应优先在内部完成，减少下游迁移 |
| 用户已授权全部实施 | 当前请求为“实施计划”；不需要再次确认常规修复与验证 |

## Errors Encountered

| 问题 | 发生阶段 | 处理结果 |
| --- | --- | --- |
| 审查期间多次 Cargo 调用达到 60 秒时限，存在共享构建锁等待 | 前期审查 | 后续核心四 crate 的 offline/locked/all-features 完整命令成功，315 项测试通过；早期不完整结果不单独计数 |
| planning-with-files session-catchup.py 在已检查的 .agents/.claude 安装路径不存在 | 计划准备 | 使用本线程审查记录恢复上下文；项目根无既有计划文件，工作区干净，不声称执行了 catchup 脚本 |
| 隔离任务共用全局 Cargo target，报告与当前源代码不一致的测试行为 | 实施验证 | 最终验证使用项目内独立 CARGO_TARGET_DIR，不依赖共享目标目录的结果；各任务停止重复宽范围检查 |
| rustfmt 报插件 match 初始化缺少分号 | 整合 | 补充分号后再次格式化；最终构建继续验证 |
| 第一轮 workspace 与 App Store Clippy 两处 op_ref（ZIP guard 比较 &magic） | 最终检查 | ZIP补修已比较直接magic；最终源重跑Clippy |
| 后台 shell 验证驱动未在工具返回后继续运行 | 最终检查 | 改为独立runner任务；未产生结果的调用不计为检查通过 |
| 示例 Cargo 自动发现外层主仓库 workspace | 最终检查 | 独立示例显式声明空 [workspace]，最终示例检查与--locked均通过 |
| DLC回归附加的Downgrade诊断断言失败（选包结果正确） | 最后复核 | 删除过严的诊断断言，保留真实计划结果断言；最终受影响检查通过 |
| 最后runner自己的target复用了旧tpk-client二进制 | 最后验证 | 仅清理runner专用target的tpk-client产物后强制编译当前父源；目标回归1项及完整矩阵通过，源diff前后不变 |
| 验收准备worker服务magpie endpoint不存在 | 阶段6 | 主线程完成实现，reviewer完成原生构建 |
| 示例注释字面script标签触发保守HTML lint | 阶段6 | 改示例注释使其符合既有门禁，实际pack验证通过 |
| 工具调用TMPDIR清除导致后续HTTPS测试缺少私钥 | 阶段6 | 私钥改放显式稳定OS临时根目录，拒绝Git内位置；最终6项通过 |
| GUI状态缺少rolled_back无法确认真实回滚 | 阶段6审查 | 加入状态显示/回归，重生成v4签名包与原生app |
| 真实墓碑fetch返回200首页而非404 | 设备验收 | 添加responseguard+宿主接入，6项新测试和编译通过，真实响应待前台复测 |
| AX扫描20s超时，限制深度也失败 | 设备验收 | capture-only+截图坐标，落盘状态交叉验证，不把unknown当通过 |
| 旧空白实例终止被Cua foreign_process_termination_denied拒绝 | 设备验收 | 未绕过，后续使用持久MCP只管理自己启动的新实例 |
| 新实例背景输入off_space_or_ax_unresolved | 设备验收 | 停止此路径，需用户显式前台控制授权；已清理服务和证书信任 |
| PATH的sample指向损坏Python命令 | 诊断 | 改用/usr/bin/sample，主线程正常事件等待；前台后能正常显示 |
| 新app验证仍200，实际启动旧登记路径 | 桌面验收 | 精确lsof核对加载路径、在target归档旧包并更新canonical生成包，真实404通过 |
| 临时子任务CLI路径被清理导致publish ENOENT | 桌面验收 | CLI重新编译到父工作树持久target，修正本地metadata路径 |
| 恢复前台时未指定window_id产生歧义 | 清理 | 仅选择唯一可见ordinary窗口，exact-window/active验证通过 |
