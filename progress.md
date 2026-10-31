# TPK 优化进度

## Session 1: 只读审查（已完成）

- 范围：tpk-format、tpk-client、tpk-store、tpk-resolve、插件状态与调用边界、CI 和发布测试。
- 整理 F01–F13；区分代码确认、scratch 复现、规范缺口及本地权限前提。
- ZIP 重复条目/原始条目上限、commit 写入失败重试、patch floor、临时 symlink 进行了 scratch 复现，未向项目加入这些探针。
- 核心四 crate all-features/offline/locked 测试最终 315 项通过；发布脚本 14 项通过；cargo fmt 检查通过。
- 部分早期 Cargo 调用受到编译锁和 60 秒时限影响；最终成功结果见 findings.md，不将不完整输出当成通过。
- 未修改产品代码，审查结束工作区干净。

## Session 2: 优化计划（已完成）

用户请求：使用 planning-with-files 根据上述发现制定优化计划。

- 检查项目根目录，没有既有 task_plan.md/findings.md/progress.md；无额外 AGENT.md/AGENTS.md；开始时 Git 状态为空。
- 已检查 .agents/.claude 两处 skill 脚本位置，未发现 session-catchup.py；依据当前线程完整记录恢复上下文。
- 创建 task_plan.md：五个实施阶段、优先顺序、验收条件、兼容性约束、预计 7–11 个开发日。
- 创建 findings.md：13 项发现的稳定编号、证据等级、代码位置和测试基线。
- 创建 progress.md：恢复与交付记录。
- 制定计划不等于开始实施；五个实施阶段保持 pending。

## Session 3: 实施（进行中）

- 用户已授权实施完整计划，读取了三个计划文件，初始仅有三个未跟踪的规划文档。
- 分配独立任务：tpk-format 负责原始 ZIP 边界；tpk-client 负责网络/频道/最终版本规划；tpk-store 负责事务发布、GC 保留、原子写与尽力恢复。
- 主线程为插件增加异步操作锁，统一协调 check/download/auto-update/reset。成功 stage 消费 pending 计划；reset 等待在途操作后清除其结果。
- 插件将接入 verify_channel_for/runtime_http_client、observe_channel、retain_runtime_layers 和 materialize_stack_best_effort 接口；当前子代理隔离工作树尚未合入，暂不运行依赖这些接口的构建。
- 插件增加重复并发下载和 reset 等待下载回归测试。仅测试配置可以注入本地 HTTP 客户端，生产下载保持 HTTPS-only。
- ZIP 中央目录预算选择 16 MiB，规范同步由主线程完成。
- 已同步 spec 和现有 API/disk-layout/security/configuration 文档，无新增 docs 页面，网站现有 symlink 继续展示修改。
- pnpm frozen 安装及 API build 成功（工具对 TypeScript 7 API 给出实验性提示，构建成功）；发布脚本 node --test 14/14 通过。
- 客户端任务已报告 59 单元 +11 signed-channel 聚焦测试通过；待隔离修复合入后执行整体检查。
- 独立 reviewer 已静态检查插件协调、Send/锁作用域、pending 消费、并发测试顺序和运行资源保留接入，未发现可操作的正确性问题；未构建尚未合入的核心接口。
- 验证六篇更新文档均通过已有 symlink 与网站内容一致。
- 三组核心实现均已合入。ZIP 本地隔离 target 的27容器/golden/roundtrip测试通过；store最后聚焦102项通过；client60单元+11频道及Clippy通过。
- 主线程后台 shell 验证驱动未在工具返回后保持运行（无结果文件/进程），不计为已运行检查。已改用独立 reviewer 负责完整矩阵，在其本地 target 顺序执行，避免反复60秒中断。
- ZIP/客户端与存储两组独立静态审查完成并发现5个回归/耐久缺口；插件协调静态审查已通过。
- 追加修复：低版本无效base不应遮蔽合法patch；合法ZIP extra中的footer magic不应误报；stage只对incoming id执行floor；回滚必须先耐久保存condemnation再丢booting；复用资源在提交前重试目录sync。三个独立worker分别负责规划、ZIP guard、store补修，原完整矩阵继续验证编译/platform，后续按新版本重跑受影响检查。
- 当前整合版本 cargo fmt --all --check、git diff --check 和六篇网站symlink一致性检查通过。
- 第一轮独立target完整 workspace all-features 测试通过：464项、0失败，1项doc test ignored；插件55项包括2项新增并发测试，store102项全部通过。该快照尚不含后续5个审查补修，需对最终源重验受影响检查。
- 规范和disk-layout补充回滚黑名单先写顺序、资源目录同步及incoming-id下限范围。
- 规划补修已合入：反向DAG计算可达最终版本，替代base与patch分支在改变投影之前检查terminal floor，O(n log n)；新增6项回归，聚焦37项通过。
- 矩阵runner更正第一次workspace测试汇总为464项（此前报564为求和错误），记录已修正；插件55与store102分项不变。
- 第一轮矩阵结束：workspace464项通过；AppStore460项通过；fmt、1.88 workspace check、AppStore all-targets check、Rustdoc -D warnings均通过。两种Clippy失败于已补修的ZIP op_ref；示例失败于外层workspace归属，主线程已添加独立[workspace]。第一轮不作为补修后最终源的验收。
- ZIP补修已合入：合法metadata扫描不误报，65535 ZIP32边界兼容，op_ref修正；format all-features138项和两种container_structure12项通过。
- store补修107项通过（45unit+61状态机+1schema），own-crate Clippy --no-deps通过；已合入最终矩阵。
- 第二轮最终矩阵10项全部通过：workspace477项、AppStore473项，Clippy两种配置、fmt、MSRV1.88、独立示例（含--locked）、AppStore all-targets check、Rustdoc -D warnings均通过。示例Cargo.lock正常更新minisign0.3与六个windows-sys引用，已同步主工作树。
- 最后规则复核补齐DLC层序：只有base/patch可以使用未来patch suffix达到floor；DLC自身必须达到floor。增加1项回归，规范修正此前逐包floor措辞与终点规则的冲突。最终受影响检查已交由独立runner重跑，未把第二轮旧源结果当作该最后改动的验收。
- 主线程对示例执行cargo metadata --locked --offline --no-deps验证workspace_root独立且锁文件不被修改。
- DLC回归实际选包结果已正确，但附加诊断断言过严（实际应先NotNewer），已删除冗余断言。runner将只读构建主工作树最终源，复用其专用target并核对tracked diff不变，避免再创建一次冷编译工作树。
- 最后主线程fmt/diff检查通过；7篇网站文档symlink一致，示例minisign锁定版本/校验和与workspace一致。

## Final Validation（完成）

最终源（含DLC层序和修正后的诊断断言）通过：

- `cargo test --workspace --all-features --locked`：478通过、0失败、1 doctest忽略。
- `cargo test --workspace --features app-store,test-packs,test-keys --locked`：473通过、0失败、1 doctest忽略。
- 两种特性配置下 workspace/all-targets Clippy `-D warnings` 通过（追加 `--locked`）。
- `cargo +1.88 check --workspace --all-features --locked` 通过。
- `cargo check --manifest-path examples/shell-app/src-tauri/Cargo.toml --locked` 通过。
- `cargo fmt --all --check` 通过。
- DLC目标回归明确1项通过。runner使用专用target，遇旧client产物后仅清理自己target的该crate并强制重编当前父源；父工作树full tracked diff及stat前后字节一致。

前一轮同公共接口实现已通过精确原命令的 AppStore all-targets check、Rustdoc `-D warnings`、示例普通check；最终追加 `--locked` 防止验证改锁。最后变化只涉及私有规划helper与其测试，以上受影响完整检查已重验。

本轮另外通过：pnpm frozen install/API build、发布脚本14项测试、7篇网站symlink同步、最终diff检查。示例生成schemas已由已有gitignore覆盖，不计为源修改。

未执行：原生GUI真实冷启动、移动实机、真实掉电测试。状态机与mock测试不替代这些设备验收。

## Session 4: 继续设备验收（进行中）

- 用户请求继续实施，恢复三份规划文件，原有未提交修改完整保留。
- 当前为macOS，Xcode安装在/Applications/Xcode.app；simctl和cua-driver存在；当前PATH未发现adb/cargo-tauri，Rust iOS target尚未安装。
- 示例使用静态src作为embedded frontend，但默认配置仍是占位公钥和CDN URL；现有本地流程依赖mkcert安装系统信任，不能直接当作已具备HTTPS条件。
- 已读取cua-driver技能及MACOS补充，桌面操作必须snapshot前后验证、使用launch_app，权限不足不绕过；移动预备条件交由只读scout确认。
- 桌面Accessibility/ScreenRecording授权为true，Cua daemon运行；未启动任何测试应用、未注册应用或修改证书信任。
- mobile scout确认Xcode27、iOS26.5/27模拟器可用但均关闭，没有连接实体iPhone/iPad；缺Rust mobile target与生成项目；Android SDK/JDK不可用。
- 准备工具worker因magpie API endpoint not found失败，主线程直接实现smoke.mjs/前端修正；reviewer构建任务正常。
- 初次真实打包发现示例注释中的字面script标签触发现有保守lint，修正示例注释，保持CLI检查规则。
- 初次HTTPS测试发现工具TMPDIR在调用结束后清除私钥；增加--private-root和Git祖先检测，在/private/tmp独立0700目录保留测试私钥（文件0600），不写入Git。
- 实现准备/发布/HTTPS服务/认证stop命令；独占发布锁、递增watermark、不可覆盖编号清单、Range/private-file隔离。服务器控制token只写私有临时目录。
- 示例捕获IPC错误、恢复失败下载重试、两次RAF后确认ready；只在生成的测试配置授予reset能力。guest-js CI加入前端回归。
- 最终测试5/5通过：4前端行为+真实HTTPS/Range/签名场景/发布锁和孤儿清单/认证停止；普通CI模式4通过1个可选HTTPS测试跳过。Node语法及diff检查通过。
- desktop-validation-v3生成并验证base/patch/broken，patch确有delta+delete。OpenSSL验证server证书链与localhost名称通过。
- 最终原生app build --debug --bundles app --config成功；包已复制到父工作树target/device-smoke/TPK Smoke s83bcd8b3ce8e.app，Info.plist标识com.example.tpk.smoke.s83bcd8b3ce8e。
- 早期v3CA未安装信任，现改用下面最终v4会话。
- docs/local-testing与示例README同步可复现流程、停止/信任撤销和移动限制；现有网站symlink覆盖页面。
- 独立静态reviewer确认私钥/发布锁/HTTP隔离/Range/认证stop安全路径，指出GUI缺少rolled_back可观测性，主线程加状态显示与回归，重新生成v4。
- 最终v4全测试6/6通过；普通CI模式5通过1个可选HTTPS跳过；Node语法、diff、网站symlink检查通过。未重跑未改动的Rust库全矩阵，保持上一阶段478/473记录。
- 最终v4 native build --debug --bundles app --config成功，源和嵌入JS均含rolled_back，tracked diff/产物源哈希不变；包复制到target/device-smoke/TPK Smoke s14ebc6960523.app。
- 最终Bundle ID：com.example.tpk.smoke.s14ebc6960523；公有CA：examples/shell-app/test-server/desktop-validation-v4/ca.crt，SHA256 04:5B:08:A2:BA:0B:79:DF:48:A9:38:42:9F:4F:3B:F5:54:21:F0:89:23:67:58:C6:88:FA:EB:AC:AC:36:64:BA，2026-10-12 10:29:13Z到期。OpenSSL验证链及localhost名称通过。
- 后续授权操作：仅用户域SSL/localhost信任（不使用-d管理员域），运行上述独立app做冷启动/回滚/reset/cache恢复，结束撤销信任。尚未执行信任导入、应用注册/启动、真实GUI或移动设备验收。

## Session 5: 实际桌面验收（进行中）

- 用户在明确的证书/app授权问题后请求继续，已授权本次限定localhost的临时信任、独立app冷启动/reset/cache验收及结束撤销。
- 核对CA04:5B:08:A2:BA:0B:79:DF:48:A9:38:42:9F:4F:3B:F5:54:21:F0:89:23:67:58:C6:88:FA:EB:AC:AC:36:64:BA及有效期，security add-trusted-cert用户SSL/localhost导入返回0。
- 仅注册并启动独立com.example.tpk.smoke.s14ebc6960523；Cua launch报告self_activation_suppressed。HTTPS服务由后台验证任务阻塞运行，后续使用认证stop结束。
- 首次截图观察embedded页面已含Check/Download/Reset日志与rev-1 staged（此前按钮动作非本agent发出，不归因）。证实原生客户端成功获取签名HTTPS清单并暂存，运行页仍embedded。
- Agent使用同一MCP连接snapshot→invoke_menu Quit→verify窗口关闭，随后launch新PID。界面真实显示TPK Smoke base 1、rev-1、pointer committed，日志notifyReady committed。
- Cua对WebView文本verify_state返回untrusted_source/unknown，不计为工具验证成功；agent直接读取界面/截图并结合专属app state.json验证。已发布patch watermark7。
- 实际补丁GUI下载rev-2并冷启动：标题TPK Smoke patch 2、pointer committed、core@1/core@2；落盘floor core2、boot_attempts0，WebView读取smoke-data.txt为200 PATCH DATA。
- 实际Reset：落盘committed/staged为空，floor2保留，pool2层与materialized结果保留；同一窗口依然显示patch2，再读delta返回PATCH DATA（LRU0/cache:no-store）。
- 墓碑实际fetch没有返回旧removed.txt，却得到200首页HTML：Tauri builtin SPA回退与纯Assets trait None的HTTP表达能力导致。新增filter_web_resource_response公共辅助函数，调用者在各webview构建前接入，最高层精确delete变empty404；不覆盖builtin协议，普通SPA/外部资源不变。
- 新guard6项与pack_assets23项通过；示例捕获原create=true配置，禁用Context自动创建，setup中from_config保存配置+hook，保留标题/标签/权限。直接percent-encoding依赖已在锁中，root/独立example lock仅增加dep引用。
- 新源例check--locked、plugin全features Clippy-D warnings、1.88plugincheck、v4debug原生appbuild通过；源文档并发改动有记录，构建本身无源变更。API/overlay/local-testing/README/spec同步opt-in和核心错误回调前失败的限制；README网站生成页已同步。
- reset后旧进程96128窗口持续空白，AX树20s超时，即使限制深度仍失败；主线程sample处于正常NSApp事件等待，不证明插件死锁，原因未确定，不能计冷启动通过。常规menu/hotkey未成功，Cua kill_app以foreign_process_termination_denied拒绝；未用shell绕过。
- 在持久MCP桥中启动新实例10855（新guarded路径）确认embedded0与floor2保留；背景Check被off_space_or_ax_unresolved明确拒绝，推荐foreground。当前授权没有包含显式前台控制，按cua-driver技能暂停此路径。
- 已通过同一持久会话结束其拥有的新实例10855，window.exists=false稳定验证成功；未强制终止旧96128。认证stop已正常结束HTTPS服务（后台任务exit0），桥stop退出且private socket消失。
- security remove-trusted-cert返回0，随后不传自定义根的system verify-cert返回CSSMERR_TP_NOT_TRUSTED，证实临时CA信任已撤销。没有持续信任遗留。

## Session 6: 前台授权后的桌面验收（完成）

- 用户明确授权临时前台操作，重新信任同一v4CA，运行本地HTTPS及Cua桥；记录原前台Ghostty。
- 将旧96128置前后能正常读取embedded页面、普通Quit成功，原空白是后台观察/操作限制，未证实产品死锁；无需绕过force-stop保护。
- 前台Check确认floor2保留时仍可下载base1+patch2完整依赖链，真实下载/冷启动重新committed成功。
- 首次404仍200，lsof准确发现LaunchServices仍启动旧canonical.app；退出后把旧生成包归档target并把guarded构建复制到canonical路径。后续真实WebView removed.txt返回空正文404，截图/AX日志确认。
- 仅删除专属smoke cache/materialized中的两个64hex常规文件（app已退出），冷启动后台重建全部结果；WebView再次返回PATCH DATA，rollback计数未增加。
- CLI原路径指向被清理的子任务target，发布坏包时ENOENT；在父工作树target/device-smoke编译durable tpk并更新本地会话metadata路径后继续，未改变签名/证书。
- broken版本3暂存rev-2，第一次真实启动pointer booting/attempts1，第二次attempts2，页面均broken3且明确no-ready；第三次恢复patch2，界面rolled back:rev-2，落盘committed rev-1/floor2/rollbacks1。
- 黑名单落盘core version_code3 reason not_acknowledged count3，再Check同一坏频道为up_to_date，Download禁用。
- 最后测试app普通退出，窗口不存在验证成功；HTTPS认证stop和桥stop均完成，socket删除；撤销CA后system verify-cert失败NOT_TRUSTED。
- 恢复Ghostty时无window_id被ambiguous_window_target拒绝，选唯一可见ordinary窗口20005后exact-window验证成功并active确认。没有读取终端内容。

## Session 7: 隔离移动构建准备（进行中）

- 桌面结束后用户要求继续，启动工作区target/ios-rustup内独立Rust1.88 minimal+sim target准备，不修改~/.rustup或全局默认工具链；构建目标放target/ios-smoke，生成项目仅ignored gen/apple。
- 验收工具支持publish --tpk覆盖已消失的旧CLI位置，保留原签名会话；使用父target稳定CLI重新prepare独立自动化夹具，6/6通过，未导入新CA或启动新app。
- 旧v4私有临时目录已被系统清理，针对它的可选HTTPS测试因ENOENT不计通过；按文档新prepare会话后的完整测试通过。

## Session 8: 移动编译诊断与可复现入口（进行中）

- 核对隔离工具链1.88.0与aarch64-apple-ios-sim已安装；RUSTUP_TOOLCHAIN必须显式设置，否则继承的1.95选择会触发无关下载。
- 原ios init停在brew reinstall libimobiledevice确认，未批准安装；仅中断自己的CLI/依赖进程，未生成gen/apple。报告的Homebrew变更建议还包括ca-certificates升级，不自动执行。
- 直接cargo check --target aarch64-apple-ios-sim --locked进入Swift桥接，Xcode27/Swift6.4默认swiftbuild后端混入macOS SDK参数而失败。
- 本地target/ios-tools/swift仅对swift build追加--build-system native --disable-keychain --disable-netrc，不改系统工具、第三方源或全局配置；同一Rust1.88模拟器跨目标check随后21秒通过。
- GitHub连接器读取公共CLI源码被403 scope拒绝；两个猜测raw路径404后改查公开contents API目录，不把失败当源码不存在的证据。

## Session 9: 固化模拟器构建入口（原生依赖阻塞）

- 当前Delta工作树为vnpe2ptg44k3，确认既有未提交实现保留；仅有gen/schemas，没有gen/apple，不依赖之前工作树的生成工程。
- 新增examples/shell-app/ios-simulator.sh，提供check/init/build；固定工作区Rust1.88、ios-rustup、ios-smoke与本地Swift native wrapper，避开版本管理器shim。脚本不会自动安装工具链。
- 实际执行`sh examples/shell-app/ios-simulator.sh check`成功：aarch64-apple-ios-sim、--locked，2秒完成。shell语法检查通过。
- `init`预检如实报告缺少idevicesyslog和pod，以exit1停止；`build`确认没有Apple工程，以exit1停止。两者均未运行Homebrew/gem安装、打开Xcode或启动模拟器。
- Tauri当前公开源码mobile/ios/project.rs调用cargo-mobile2 deps::install_all；cargo-mobile2 src/apple/deps/mod.rs明确检查xcodegen/libimobiledevice/cocoapods，缺失时brew reinstall或gem安装。--skip-targets-install只影响Rust目标；未发现原生依赖跳过选项。该源码来自dev分支，结合本机2.12.1 help和此前实际安装尝试判断，不声称检出了2.12.1完整源码。
- 本机xcodegen已存在；libimobiledevice与cocoapods既无PATH命令，也无对应Homebrew Cellar目录。完整app构建等待明确的机器依赖安装决定；不以Cargo check替代链接、模拟器运行或实机验收。
- 已更新docs/local-testing.md与示例README的隔离准备、运行入口和依赖边界；现有网站symlink同步该文档。

## Session 10: 原生依赖与Apple工程完成，runtime版本阻塞

- 用户回复继续，授权上轮具体提出的`brew install libimobiledevice cocoapods`及后续初始化/无签名构建。
- 首次brew调用自动更新Homebrew 7.0.10→7.0.11及portable Ruby，停在新版默认安装确认并达到60秒工具时限；没有把该次算安装成功。查brew install --help后使用--yes与局部NO_AUTO_UPDATE/NO_INSTALL_CLEANUP/NO_INSTALLED_DEPENDENTS_CHECK，安装成功。
- 实际安装libimobiledevice1.4.0_2、CocoaPods1.17.0_1及其依赖（含Ruby4.0.9、OpenSSL4.1.0、libplist/glue/libtatsu/libusbmuxd）；ca-certificates更新至2026-10-20。Homebrew把openssl@3的命令链接切换到openssl@4，未删除旧keg。未授信任何第三方tap，未批量升级其他已过时包。
- `HOMEBREW_NO_AUTO_UPDATE=1 sh examples/shell-app/ios-simulator.sh init`成功，生成ignored gen/apple/tpk-example.xcodeproj；未自动更新已存在的xcodegen2.45.4。`xcodebuild -list -json -project ...`确认debug/release及tpk-example_iOS target/scheme。
- 精确执行`sh examples/shell-app/ios-simulator.sh build`失败于Tauri预检：Xcode Simulator SDK27.1 is not installed。xcodebuild -showsdks及xcrun --sdk iphonesimulator --show-sdk-path均确认SDK27.1存在。
- `simctl list runtimes --json`显示只有iOS26.5与27.0；cargo-mobile2公开dev源码apple/target.rs的validate_sdk确认其实际要求available runtime.version >= 默认SDK版本。因此准确阻塞是缺27.1 runtime，不是缺编译SDK。
- 安装后重跑脚本check成功（Rust1.88/aarch64-apple-ios-sim/--locked，1.05秒）；文档补充SDK/runtime差异与明确版本下载命令。未下载/导入新runtime、未启动模拟器或修改开发者签名/测试证书信任。

## Session 11: 已授权runtime安装，后台下载及后续构建

- 用户回复继续，明确授权iOS27.1 arm64 runtime安装及随后无签名构建；无需再次请求该授权。
- 精确xcodebuild -downloadPlatform iOS -buildVersion 27.1 -architectureVariant arm64命令下载7.85GB的24A94425；首次前台调用60秒时限取消，不计安装成功。nohup方式未留存进程/日志，不计启动成功。
- 改用Python Popen(start_new_session=True)脱离PTY，PID49595经下一调用ps确认持续运行；target/ios-smoke/logs/runtime-install.log记录下载进展。最近观察39.1%，尚无安装成功退出码。
- 后续runner PID58102已安排等待runtime-install.exit；仅安装exit0时执行精确`sh examples/shell-app/ios-simulator.sh build`，保存build.log/build.exit及runtimes.json；安装失败则停止并记录runtime-install-failed。runner位于ignored target/ios-smoke/logs/continue-build.py，不启动模拟器/GUI、不改签名信任。
- 后续恢复须先读取上述日志/退出码和进程，不重复启动下载或构建；未写退出码不代表成功。完整.app及运行验收仍待核对。

## Session 12: 完整iOS模拟器.app构建通过

- 用户报告iOS27.1(24A94232)安装成功。核对simctl同时列出24A94232与后台完成的24A94425，两者available=true；没有重复下载、卸载或清理用户runtime。
- 后台runtime-install.exit为0，build.exit为1：自动构建继承已清理的Delta终端TMPDIR，创建临时文件ENOENT。两个后台任务均已结束。
- 修正ios-simulator.sh统一将TMPDIR设为target/ios-smoke/tmp并创建，文档同步；使用脱离PTY的进程重试精确`sh examples/shell-app/ios-simulator.sh build`。
- build-retry.exit为0，Tauri输出Finished 1 iOS Bundle，产物：examples/shell-app/src-tauri/gen/apple/build/arm64-sim/tpk-example.app。日志target/ios-smoke/logs/build-retry.log。
- 实际file检查为Mach-O64 arm64；Info.plist标识com.example.tpk，CFBundleSupportedPlatforms=iPhoneSimulator；vtool LC_BUILD_VERSION为IOSSIMULATOR、minos15.0、sdk27.1，确认不是macOS或实体iOS产物。
- 修正后`sh examples/shell-app/ios-simulator.sh check`再次通过，Rust1.88/aarch64-apple-ios-sim/--locked，19.23秒；Shell语法、git diff --check通过。未修改产品Rust代码，不重复此前全workspace矩阵。
- 当前仅完成示例默认配置的无签名构建；未启动模拟器、安装app、导入模拟器HTTPS信任或执行移动OTA。默认示例仍含占位公钥/URL，运行验收须使用新的独立smoke配置。

## Session 13: 独立iOS OTA验收会话构建完成

- 用户继续要求推进移动验收。prepare创建examples/shell-app/test-server/ios-validation-v1，使用稳定父target的tpk CLI、/private/tmp中的0700私钥目录、独立端口8444。
- Bundle ID com.example.tpk.smoke.s9a4fcf030737；新CA SHA256 B1:F8:B9:53:DF:2A:12:47:C5:AE:8B:34:26:C4:72:C5:F2:36:87:DD:56:53:91:43:6B:38:97:D6:13:AF:9C:2F，有效至2026-11-01 05:06:28Z。未导入任何信任。
- 新会话真实HTTPS/Range/签名等6项测试全部通过。`ios-simulator.sh build --config <本会话tauri.smoke.json>`成功，smoke-build.exit0，Info.plist核对identifier与iPhoneSimulator正确；产物路径另存target/ios-smoke/logs/smoke-app-path.txt。
- 产物gen/apple/build/arm64-sim/TPK Smoke s9a4fcf030737.app。ios TLS使用非Android native-tls配置，可使用模拟器单独root store，Mac Keychain无需新增信任。
- 已读取cua-driver及MACOS补充；权限status Accessibility/ScreenRecording=true；尚未启动Cua会话、模拟器GUI、app或持久HTTPS服务。
- 下一步需明确一次性授权创建专用模拟器/启动安装/仅其中导入本CA/必要时短暂前台操作与恢复/结束删除专用模拟器以移除信任。该操作不接触用户已有模拟器数据；尚未执行。

## Session 14: 授权模拟器运行，发现生命周期阻塞并完成清理

- 用户继续，已授权上一轮列明的专用模拟器创建/启动/安装、仅其中CA信任、必要前台操作、结束删除专用设备与停止服务；该授权不需重复请求。
- iOS27.1 runtime仅支持iPhone Duo，首次用iPhone17创建明确Incompatible device，后改用runtime报告的Duo成功。专用UDID DB7AF33B-4E2E-49A1-A3E3-DE04231FF37E，bootstatus Finished。
- keychain add-root-cert成功，安装smoke app成功；HTTPS服务8444启动；simctl launch返回PID39899，但不代表窗口持续存活。
- 实际崩溃报告TPK Smoke s9a4fcf030737-2026-10-30-132156.ips：主线程EXC_BREAKPOINT/SIGTRAP，首帧UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption，随后UIApplicationMain→tao EventLoop→Tauri→示例run。未进入OTA按钮操作，不能归因为OTA状态机失败。结构化摘录保存target/ios-smoke/logs/ios27-crash-summary.json。
- 本机Xcode27使用Contents/Applications/DeviceHub.app（com.apple.dt.Devices），没有传统Developer/Applications/Simulator.app；Cua launch Simulator返回APP_NOT_INSTALLED，按实际目录找到Device Hub并启动，PID69830。未绕过Cua调用GUI。
- 尝试独立iOS26.5 iPhone17作为运行替代，UDID09C17F02-FF74-4511-A39E-2A6EE3C22E72；首次数据迁移持续未完成，bootstatus多次时限退出，keychain/install调用未返回成功，不计安装或信任已完成。不伪造runtime/关闭UIKit检查/改第三方依赖。
- 结束：smoke.mjs认证stop成功；shutdown26.5成功；delete两个专用UDID成功，随后simctl JSON断言两者不存在且booted为空。删除设备同时移除其中已导入的测试CA与应用数据；Mac Keychain从未导入本次CA。
- Cua普通Quit菜单因目标未稳定key被拒，未绕过；使用已授权foreground cmd-q投递，list_windows随后为空，确认Device Hub窗口已退出。工具前台恢复为best-effort；开始时读取原前台命令超时，无独立前后app核对证据，不声称明确恢复验证成功。未操作用户已有模拟器数据。
- 文档补充SDK27.1/Tauri2.10.3/tao0.34.8运行限制。此轮没有修改产品Rust代码，未重复全workspace矩阵；git diff --check通过。

## Session 15: 场景生命周期上游调查与隔离新版宿主验证

- firecrawl developer与完整上游issue15719核对：iOS27验证静态UISceneConfigurations；只声明空dict不够，需要TaoSceneDelegate和支持scene的运行时。tao0.34.8无scene实现；tao0.35.3有实现但configuration_for_connecting_scene_session返回Retained::as_ptr导致对象释放后指针失效，#1245修复从0.36.0发布。官方发布记录https://v2.tauri.app/release/tao/支持该判断。
- 本机缓存元数据确认Tauri2.11.6/runtime-wry2.11.4仍使用tao^0.35；Tauri2.12.1与runtime-wry2.12.1要求Rust1.90。因此未修改正式依赖或提高项目MSRV1.88。
- 在ignored target/ios-scene-probe复制独立示例作为诊断宿主，tauri=2.12.1、插件为父路径依赖，静态Info.ios.plist声明TaoSceneDelegate；工具链安装在target/ios-scene-rustup，目标target/ios-scene-build。官方源TLS EOF后rsproxy镜像成功安装Rust1.90.0+sim target。
- 安装副作用：rustup自行从1.29.1更新为1.29.2（~/.cargo/bin中工具管理器）；隔离RUSTUP_HOME的默认设为1.90，全局rustc仍1.95未改。该自更新未在最初隔离预期内，已向用户说明；文档安装命令补--no-self-update防止再次发生。没有将该行为误报为完全无全局修改。
- 诊断init/build成功exit0，锁定tauri2.12.1/runtime-wry2.12.3/tao0.37.2。静态scene manifest已在实际.app Info.plist核对。正式example和workspace锁文件未因该实验改变。
- 专用iOS27.1 Duo设备3B2D4D81-928E-4092-9A5B-EA24EC2E87E5首启迁移报告Data Migration Failed（bootstatus进程exit0不代表系统就绪）；随后安装/启动成功，PID13124持续存活，未再触发原NoSceneLifecycleAdoption崩溃。
- Cua截图确认实际embedded HTML渲染，但状态仍Loading…、无ready日志；仅证明越过生命周期trap，IPC/OTA仍未验收。截图target/ios-smoke/logs/scene-probe.png，构建日志scene-build.log/exit；未导入新CA或启动HTTPS服务器。
- 结束shutdown/delete专用设备成功，Device Hub(PID22144)经snapshot→已授权foreground cmd-q→list_windows为空确认窗口退出。未操作用户原有设备或runtime。
- 文档补充明确版本/证据/局限及上游来源；下一步需选择符合MSRV政策的正式宿主方案，不能直接把诊断2.12升级合入项目。

## Session 16: 新版诊断宿主的iOS模拟器OTA验收完成

- 找到Loading根因：构造诊断配置时替换整个app对象，漏掉原示例withGlobalTauri=true；smoke HTML使用window.__TAURI__，因此脚本未初始化。仅修改ignored target/ios-scene-probe配置，重新无签名构建成功（scene-ipc-build.exit/log）；正式Rust1.88依赖和锁文件未升级。
- 新专用设备3592A79F-9055-4C13-8C94-D0828CA63217，iOS27.1；实际安装com.example.tpk.sceneprobe，CA仅在此设备。页面显示embedded、pointer committed、notifyReady noop，确认IPC实际返回。
- 使用ios-validation-v1本地签名HTTPS会话：base1下载40997字节，冷启动后页面base1/rev-1、notifyReady committed；落盘committed rev-1、floor1、attempts0。
- 发布patch watermark7；客户端下载6395字节后冷启动，页面patch2/rev-2、core@1/core@2、notifyReady committed。实际WebView smoke-data.txt返回200 PATCH DATA；removed.txt返回空404。截图ios-patch.png、ios-delta.png、ios-tombstone.png。
- 发布broken watermark8；版本3下载后staged rev-3，当前持久pointer仍committed（stage不改变当前运行修订；早期探针错断言pointer必须staged，已按实际staged字段核对，非产品失败）。首次真实启动booting/attempts1并显示broken3、不调用ready；第二次booting/attempts2；第三次恢复patch2，页面rolled back:rev-3。
- ios-broken-first-state.json、ios-broken-second-state.json、ios-rollback-state.json保存状态证据；blacklist.json记录core3/not_acknowledged/count3，committed rev-2/floor2/rollbacks1/attempts0。再次实际Check返回up_to_date，Download禁用（ios-blacklist-check.png）。
- Cua全程沿用已授权窗口/前台操作；早期坐标缩放错误未触发按钮，查describe后按PNG像素投递成功。会话过期拒绝不计操作成功；后续使用新生命周期标签。WebView内容不在Device Hub AX树中，按截图与落盘状态交叉验证，不将动作unverifiable算验收。
- HTTPS认证stop成功；专用设备shutdown/delete成功，CA随设备删除；Device Hub PID66918经snapshot→foreground cmd-q→list_windows为空确认退出；持久桥stop成功。simctl list devices booted为空。前台恢复由Cua best-effort处理，本轮未独立证明原前台恢复；Mac信任未更改。
- 本轮未在模拟器重验reset/cache清理恢复，未验移动实机/Android。全部通过结果限定Rust1.90/Tauri2.12.1/runtime-wry2.12.3/tao0.37.2诊断宿主，正式Rust1.88示例iOS27生命周期阻塞仍未解决。没有提交或推送。

## Session 17: 模拟器缓存恢复、reset与保留下限重装通过

- 继续使用ignored Rust1.90/Tauri2.12诊断.app和ios-validation-v1签名会话，证书有效期核对通过；发布patch watermark9。新专用iOS27.1设备74B0E907-3F7F-4A35-8CC0-09974AACE31F，CA仅导入此设备，没有改Mac信任。
- 首次迁移完成后安装/运行；实际Check选base1+patch2共47392字节并下载，冷启动提交patch2/rev-1、floor2/attempts0。修订编号从新设备开始，因此此轮patch2为rev-1，不与上轮rev-2混同。
- 关闭app后仅删除该app容器materialized目录两个64hex常规文件，先记录内容SHA256，确认目录为空；下次真实冷启动两者全部重建，内容哈希完全一致、consecutive_rollbacks0。实际WebView重新读取smoke-data.txt为200 PATCH DATA。证据reset-cache-manifest.json、reset-cache-rebuilt-state.json、reset-cache-resource.png。
- 实际点击Reset：落盘committed/staged均空，floor2保留，两个运行中delta结果仍在；当前页面仍patch2，再次点击读取资源返回200 PATCH DATA（cache预算0，smoke fetch no-store）。证据reset-applied-state.json、reset-applied.png、reset-live-resource.png。
- 冷启动实际页面embedded0、无layers；floor2仍在。再次Check选完整base1+patch2链，下载stage后冷启动提交成功，floor2/attempts0/rollbacks0。首次10秒观察窗口未等到ready，不算失败；后续实际页面与落盘committed确认通过。证据reset-cold.png/state.json、reset-reinstall-check.png、reset-reinstalled.png/state.json。
- 本机Cua工具接口已更新，拒绝旧element_index/snapshot_id参数；查describe后使用snapshot绑定的element_token，不将拒绝算成功。像素动作使用同一会话的实际截图；仅管理本桥启动的Device Hub PID92472。
- HTTPS认证stop成功；专用设备shutdown/delete成功，测试CA随设备移除；Device Hub经snapshot→已授权foreground cmd-q→list_windows为空确认关闭，Cua桥stop成功。前台恢复由工具best-effort处理，未独立核对原前台。
- 正式MSRV/依赖与产品源未因本轮验证改变，未提交或推送；更新既有文档及网站symlink。移动实机、Android、真实掉电仍未验收，正式Rust1.88示例iOS27生命周期兼容仍阻塞。

## Current State

桌面真实验收全部目标通过。新版独立诊断宿主iOS27.1的签名base/patch、delta、空404、未ready回滚/黑名单、缓存清除重建、reset冻结窗口及保留下限重装通过；证据见Session16–17。专用设备和服务均已清理。正式项目保持Rust1.88，官方示例在iOS27的宿主生命周期兼容仍阻塞。

## Next Action

正式iOS27支持需独立决定宿主方案：允许仅示例宿主采用Rust1.90/Tauri2.12，或评估旧工具链兼容的上游回补/旧SDK。不能以诊断宿主通过为依据直接提高插件MSRV。移动实机、Android与真实掉电仍未验收；模拟器reset/cache恢复已在Session17完成。
