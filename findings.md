# TPK 审查发现

## Evidence Policy

本文件记录前一轮只读审查证据。代码路径和行号对应审查时版本，实施前重新核对。代码确认、scratch 故障注入和端到端验证是不同证据等级，不混用。315 项既有测试通过不意味着这些边界已覆盖。

## Findings

| ID | 优先级 | 问题与触发条件 | 证据与位置 | 实施阶段 |
| --- | --- | --- | --- | --- |
| F01 | P1 | commit_booting 先改内存再 persist；写盘失败后重试 Noop，磁盘仍 booting，可能回滚已 ready 修订 | scratch 故障注入已复现；crates/tpk-store/src/store.rs:341–351 | 1 |
| F02 | P1 | reset 清空修订并 GC，删除当前冻结 resolver 仍需的 delta；未缓存读取会回退 embedded | 调用路径确认；store.rs:428–450、551–565；plugin state.rs:394；未做 WebView 端到端复现 | 1 |
| F03 | P1 | zip 8.6.0 在构造 archive 时合并重复名，项目的重复检查和原始条目计数失效 | scratch 已接受重复 manifest 和 65,537 同名条目；crates/tpk-format/src/container.rs:71–104；spec/tpk-v1.md:135 | 2 |
| F04 | P1 | 未绑定 manifest.channel 与 config.channel，共钥频道之间可错装内容并污染配置频道水位线 | 代码确认；crates/tpk-client/src/lib.rs:37–58；plugin state.rs:155–167 | 3 |
| F05 | P1 | 无 Content-Length 时 bytes().await 收齐后才检查 4 MiB，验签前可能消耗大量内存 | 代码确认；crates/tpk-client/src/source.rs:184–203 | 2 |
| F06 | P2 | 脱敏前缀后拼原始 reqwest Error，其 Display 仍可能包含查询 token | 当前依赖实现确认；source.rs:175、download.rs:173；现有测试仅覆盖 finalize 哈希错误 | 2 |
| F07 | P2 | 自动/手动下载未统一协调，共享 hash.part/hash.tpk，清理可能删除另一调用使用的文件 | 代码确认；plugin state.rs:213–306、470–498；未做并发端到端复现 | 4 |
| F08 | P1 | patch 分支不检查 version_floor，可选择低于已提交下限的最终内容 | scratch：installed1/floor3/patch2 parent1 得 Apply；crates/tpk-client/src/plan.rs:268–291；spec:334 也只约束非 patch，属于规范与实现共同缺口 | 3 |
| F09 | P2 | 默认 HTTP client 无显式连接/读取/总超时，非 HTTPS-only，初始 URL 检查不能阻止重定向降级 | reqwest 0.12.28 默认配置及调用点确认；source.rs:143、plugin state.rs:248 | 2 |
| F10 | P2 | atomic_write 忽略父目录打开和 fsync 错误，可能虚假报告耐久成功 | 代码确认；crates/tpk-store/src/state.rs:357–359；未做掉电测试 | 1 |
| F11 | P2 | materialize_stack 首个失败即退出，阻止后续独立 id 恢复；setup 线程仅尝试一次 | 代码确认；crates/tpk-store/src/materialize.rs:190–192；plugin lib.rs:207–224 | 4 |
| F12 | P2 | 合法结束但不足完整包的短 206 会 finalize 删除有效前缀；未验证 Content-Range | 代码确认；download.rs:184、224、60–63 | 4 |
| F13 | P2，条件性 | 可预测临时名配合 File::create 可跟随预置 symlink，截断根外目标 | scratch 已复现；crates/tpk-store/src/state.rs:332–338；需本地/同进程组件具有 store 目录写权限，并非远程 Zip Slip | 1 |

## Important Boundaries

- F03 未证明签名绕过：胜出的 manifest 仍需要有效签名，blob 仍校验 hash；确定失效的是 ZIP 结构约束及原始条目资源预算。
- F08 需要更新冻结规范。实施前区分必要中间 patch 与最终激活版本，并维持正常失败回滚规则。
- F01 涉及多文件持久化时，不能简单“捕获所有错误后一律恢复旧内存”：必须辨明 state 是否已经耐久提交。
- reset 的正确修复应保留当前运行资源，不能在运行 WebView 中直接替换 resolver。
- 网络签名保护不等于资源保护；F05 消耗内存发生在验证签名之前。

## Implementation Decisions

- 用户已授权实施 F01–F13。独立核心 crate 修复并行，插件集成和规范由主线程负责。
- 生产 HTTP 客户端 HTTPS-only，测试通过专用注入使用本地 HTTP fixture，避免为了测试放宽运行时策略。
- 插件 check/download/auto-update/reset 统一异步协调；reset 等待完成，成功下载消费计划，保留现有前端命令签名。
- ZIP 原始中央目录预算 16 MiB；原始条目和 ZIP64 检查必须先于依赖初始化。
- 规范将明确最终 patch 投影不得低于 floor、运行引用 GC 保留、原子写平台耐久限制以及独立层尽力恢复。

## Independent Review Follow-ups

五个补修均属于原发现修复的正确性验收，不新增产品范围：

- F08：无效的低于floor替代base不能遮蔽现有层可达的合法patch链；新增竞争base回归。
- F03：ZIP metadata扫描窗口落在合法extra内的EOCD magic时不能误判为备用footer解析；保留真实fallback防护回归。
- F08/store：只约束incoming更新id的最终版本；seed豁免导致低版本的继承id不能挡住无关更新。
- F01/blacklist：回滚丢弃booting之前须先耐久保存新condemnation，避免进程退出后丢失失败包身份。reset清空仍先state再blacklist。
- F10：即使资源已存在，stage发布state前仍须同步layer/materialized目录，修复上次rename成功而目录fsync失败的耐久性。

## Existing Strengths

- CI 覆盖 Linux/macOS/Windows、MSRV 1.88、显式 App Store 特性、独立示例和包装检查。
- 冻结格式规范、Golden fixture、JSON schema 一致性测试已经存在。
- 核心状态机、layer overlay、签名与黑名单有较完整的正常路径测试。

## Final Implementation Status

F01–F13及独立审查补修均已完成：

| 修复组 | 已实现行为 |
| --- | --- |
| F01/F10/F13 | 候选状态耐久后发布；回滚condemn先写；失败可重试；资源目录复用补sync；独占临时创建 |
| F02/F11 | 运行resolver层和delta结果GC保留；reset不破坏当前资源；启动恢复继续独立层 |
| F03/F05/F06/F09 | 原始ZIP/ZIP64有界校验；合法metadata magic与ZIP32边界兼容；流式大小预算；HTTPS/超时和网络错误脱敏 |
| F04/F08 | 签名频道绑定；可达终点先于替代/投影；DLC层序约束；incoming最终版本floor且保留seed/回滚豁免 |
| F07/F12 | check/download/reset/auto-update异步协调；成功stage消费计划；验证Content-Range并保留合法短前缀 |

最终自动化：workspace/all-features478项、AppStore473项测试通过；两种Clippy -D warnings、MSRV1.88、示例--locked、fmt通过。公共接口对应实现Rustdoc -D warnings通过，TypeScript构建和发布脚本14项通过。最终新增DLC目标回归1项通过。详细实际命令和快照区别见progress.md。

兼容性变化已写入spec：16MiB ZIP元数据预算，EOF footer/绝对偏移/连续中央目录；Golden fixture保持兼容。新增API为兼容式扩展，无新crate或MSRV变化；前端命令签名保持不变。

尚未验证原生GUI冷启动、移动实机、真实掉电；不要把mock/状态机测试视作这些验收通过。

## Device Acceptance Preparation

- 新增examples/shell-app/smoke.mjs与smoke.test.mjs：生成可信测试签名、真实delta/tombstone、故意no-ready包、独立app配置及短期localhost证书；密钥与服务器控制token只在仓库外私有临时目录。
- 前端原来的DOM赋值后立即ready不足以确认绘制机会，现等两次RAF；错误/retry处理与rolled_back显示均有回归。保守HTML lint会扫描注释，示例注释已改成普通文字，实际目录成功打包。
- 六项测试全部通过，包含进程内CA的真实HTTPS/Range、发布互斥/不可变孤儿清单和认证停止。这不证明原生Rust客户端已信任测试证书。
- 最终v4原生macOS.app已构建，id com.example.tpk.smoke.s14ebc6960523。真实GUI/cold-start等待用户授权临时localhost SSL信任；没有导入证书或启动app。
- macOS GUI权限可用；iOS模拟器存在但缺Rust mobile target/生成项目、无实体iOS设备；Android SDK/JDK不可用。Android rustls使用bundled roots，不能将桌面或Android用户CA导入误当作native updater信任。

## Native Desktop Evidence and Remaining Acceptance

实际已验证（独立v4 macOS app，不是mock）：

- 签名HTTPS清单/包下载，embedded页保持不热切层，base rev-1冷启动后ready committed。
- patch rev-2冷启动后committed/floor2，WebView delta读取返回PATCH DATA。
- Reset清空持久修订保留floor2/运行层/物化结果，同一窗口仍是patch2，LRU0/cache:no-store再次返回PATCH DATA。

实际发现并补修：`removed.txt`墓碑正确隐藏旧payload，但Tauri builtin SPA返回200首页。新增TpkHandle响应guard、示例创建窗口时接入，6项guard和23项pack_assets通过，Clippy/MSRV/例check/nativebuild通过。随后在已授权的前台操作中，确认LaunchServices实际运行旧包并修正登记路径；真实WebView复测返回空正文404。

桌面后续验收完成：保留下限2时重装base1+patch2；删除专属物化缓存后冷启动重建并返回PATCH DATA；broken3连续两次未ready后第三次回滚patch2，黑名单记录not_acknowledged，再检查up_to_date。旧空白实例置前后正常显示并普通退出，未证实死锁。全部测试实例/服务/桥已停，CA信任撤销且NOT_TRUSTED验证，原前台恢复。详细证据见progress.md Session 6。

## iOS Simulator Build Boundary

- Rust1.88与aarch64-apple-ios-sim安装在工作区target/ios-rustup；ios-simulator.sh check以--locked通过，SwiftPM native后端仅由局部PATH包装器选择。
- 用户授权后已安装libimobiledevice/CocoaPods及依赖；init生成gen/apple成功，xcodebuild -list可解析debug/release及iOS scheme。现已生成完整无签名gen/apple/build/arm64-sim/tpk-example.app。
- 初次build被runtime版本预检阻塞；iOS27.1实际安装后解除。后台构建另遇继承的终端TMPDIR被清理，脚本统一使用target/ios-smoke/tmp修复。精确脚本build重试exit0，file/Info.plist/vtool确认arm64、IOSSIMULATOR、SDK27.1、minos15.0；完整证据见progress.md Session12。
- Tauri --skip-targets-install只跳过Rust目标；原生依赖已由授权的Homebrew安装处理。未启动模拟器或改变开发者签名/测试证书信任；安装副作用见progress.md Session10。
- 完整.app链接已验证；真实iOS27.1(24A94425)启动随后EXC_BREAKPOINT/SIGTRAP，主线程UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption→UIApplicationMain→tao/Tauri。属于当前宿主生命周期兼容性阻塞，OTA按钮尚未触发。iOS26.5替代设备首次迁移未完成，不能计为运行通过。
- 本次运行已获授权，两个专用模拟器及其中CA/数据均已删除，HTTPS服务已停止，Device Hub窗口已退出；没有改Mac证书信任或原有模拟器。前台恢复仅工具best-effort，没有独立前后验证；详细证据见progress.md Session14。

## iOS Scene Lifecycle Investigation

- 上游tauri#15719说明iOS27静态scene manifest要求；tao0.34.8缺scene实现，0.35.3还含scene配置返回指针的use-after-free，#1245修复发布于0.36.0。只加plist或只升级至Tauri2.11不足以覆盖已知问题。
- Tauri2.12.1要求Rust1.90，项目MSRV1.88不自动放宽。在ignored target中构建的独立Rust1.90诊断宿主实际使用Tauri2.12.1/runtime-wry2.12.3/tao0.37.2，并声明TaoSceneDelegate。
- 首轮新版宿主页面Loading的原因已确认：替换整个app配置时漏掉withGlobalTauri=true，smoke页面依赖window.__TAURI__。在ignored诊断配置中补回并重新构建后，IPC正常；正式锁文件/依赖未升级。
- 后续专用iOS27.1模拟器真实验证embedded→base1→patch2冷启动提交；WebView读取delta为200 PATCH DATA，墓碑为空404。坏包3两次booting/attempts1、2，第三次恢复committed rev-2；黑名单not_acknowledged/count3，floor2保留，同频道再次check为up_to_date。证据为target/ios-smoke/logs/ios-*.png与状态JSON，详情见progress.md Session16。
- 后续独立设备补齐reset/cache恢复：关闭app后清除两个materialized结果，冷启动哈希一致重建，WebView重新读到PATCH DATA且无回滚；reset清空持久修订并保留floor2，当前窗口仍读PATCH DATA，下次冷启动embedded；保留下限仍可选base1+patch2完整链重新下载并提交。证据见progress.md Session17与target/ios-smoke/logs/reset-*。
- 仅新版Rust1.90诊断宿主的上述场景通过；正式Rust1.88示例生命周期兼容仍阻塞，移动实机与Android未验收。两轮专用设备3592A79F-9055-4C13-8C94-D0828CA63217与74B0E907-3F7F-4A35-8CC0-09974AACE31F均已删除，HTTPS与Cua桥已停，Device Hub窗口退出；没有改Mac证书信任。
- 工具链安装器发生rustup自更新1.29.1→1.29.2，已披露并在文档补--no-self-update；不等于项目MSRV或全局默认Rust工具链改变。详情见progress.md Session15。

## Baseline Validation

完整通过：
```sh
cargo test -p tpk-client -p tpk-format -p tpk-store -p tpk-resolve --all-features --offline --locked
cargo fmt --all --check
node --test scripts/content-release.test.mjs
```

结果：核心测试 315 项通过，发布脚本 14 项通过，格式检查通过。审查结束时工作区干净。

未完成或未运行：完整 workspace 测试、workspace Clippy、MSRV 实际构建、TypeScript 构建、移动实机和真实掉电测试。子代理早期受时限影响的 Cargo 部分输出不计为完整成功。
