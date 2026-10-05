# MP3 下载、歌词导出与文件管理 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** 支持单曲、多选、完整歌单和专辑下载为 MP3，导出多格式歌词，并在本地或 TF 卡等保存目录内管理及删除文件。

**Architecture:** 后台下载管理器负责队列和持久化，API、音频、歌词与安全文件操作使用独立模块。现有 Action 通道连接原生 GTK / Libadwaita 对话框和页面，主线程只处理界面。项目内维护 ncm-api 2.2.0 的小范围补丁，补齐完整歌单、试听信息和流式传输。

**Tech Stack:** Rust 2024、GTK4 0.11、Libadwaita 0.9 / v1_5、GStreamer 0.25、async-channel、serde JSON、GIO、encoding_rs、id3、futures-lite、rustix。

**Spec:** `docs/superpowers/specs/2026-10-04-song-download-design.md`

## Global Constraints

- 保留 `adw` 的 `features = ["v1_5"]`；所有 UI 使用 GTK 4.14 / Libadwaita 1.5 可用控件。
- MP3 固定码率 128 / 192 / 320 kbps，默认 192；44.1 kHz、双声道，基本标签使用 ID3v2.3。
- 默认原文 LRC / UTF-8；LRC、TXT、SRT 可多选；UTF-8 无 BOM，换行 CRLF；GBK 编码失败不能替换为问号。
- 默认建议系统音乐目录下 `NetEase Downloads`，无系统音乐目录时为 `~/Music/NetEase Downloads`；由用户确认保存位置后才写入。
- 文件名片段最多 180 UTF-8 字节；默认 `歌手 - 歌名`；编号至少三位，来自入队快照。
- 首版不覆盖已有文件、不自动重启任务、不做断点续传、设备同步或本地曲库播放。
- 初始并发两个歌曲任务、一个编码管线；每任务进度最多每秒四次。
- 任务历史、文件索引及临时文件清单独立维护；TF 卡只保存输出文件。
- 清理和删除仅处理任务拥有或显式纳入的文件；回收站失败不得自动永久删除。
- UI 复用页面栈、boxed-list、flat / circular 按钮、AdwClamp、原生分组设置和 gettext；不添加侧边栏或新配色。
- 测试只写临时目录，不对真实用户歌曲做删除测试；网络测试使用本地受控服务。

## Review Focus

- 卡拔出后原挂载目录仍然存在：不得把文件写入电脑上的旧路径；由 Task 4 / 6 的身份失效测试覆盖。
- 同大小、同修改时间的文件被外部替换：删除须发现内容指纹变化；由 Task 4 的替换文件测试覆盖。
- 连续点击下载以及歌单含重复 ID：去重后保留正确顺序和可见数量；由 Task 3 / 6 的重复快照测试覆盖。
- 多选模式中歌曲行仍收到释放事件：不得误播放；由 Task 8 的实际控件事件测试覆盖。
- 音频已保存、部分歌词失败后重试：不得重复覆盖音频或丢失已成功输出；由 Task 6 的部分完成重试测试覆盖。

## 文件结构和共用类型

新增 `src/lib.rs` 导出 `download` 模块，二进制从库引用该模块，避免在测试和界面中重复编译两份实现。库只接收配置路径和 MusicApi，不引用二进制的 MAINCONTEXT 或窗口类型。

- `src/download/{mod,model,naming}.rs`：公开接口、选项、输出结果和命名。
- `src/download/lyrics.rs`：时间轴解析、翻译组织及编码。
- `src/download/api.rs`、`vendor/netease-cloud-music-api/`：资源请求与 API 小补丁。
- `src/download/{storage,catalog}.rs`：根目录身份、文件提交 / 删除 / 扫描、JSON 数据和指纹。
- `src/download/audio.rs`：媒体探测、转换和标签。
- `src/download/{manager,worker}.rs`：队列、生命周期、后台流程和进度。
- `src/gui/{download_dialog,downloads_page}.rs` 和对应 `data/gtk/*.ui`：原生对话框与两视图管理页。
- `tests/download_{naming,lyrics,api,storage,audio,manager,ui}.rs`，`tests/common/mod.rs`：行为测试和仅供测试使用的临时目录 / 服务夹具。

Task 1 定义以下公共类型，其余任务不另造同义类型：`TaskId = u64`、`LocationId = String`；`DownloadOptions` 含 `target: TargetLocation`、`bitrate: u32`、`content: DownloadContent`、`lyric_formats: Vec<LyricFormat>`、`lyric_mode: LyricMode`、`encoding: TextEncoding`、`collection_folder: bool`、`numbered: bool`。`DownloadPreferences` 使用尚未确认的 `target_path: PathBuf` 和同样的输出参数，供设置与对话框使用，提交时才创建允许的新本地目录并捕获 TargetLocation；失效的可移除位置不允许自动创建。`TargetLocation` 含 ID、规范化根路径、文件系统 ID、可移除标记、可选卷 UUID 和会话挂载标识。`CollectionSnapshot` 含 ID、名称、来源类型和按顺序排列的 `CollectionItem { song_id: u64, song: Option<SongInfo>, error: Option<String> }`，确保无法获取详情的条目也能展示失败结果。

`StoredFile` 含相对路径、文件种类和 `Fingerprint`；`SavedSong` 含位置、歌曲信息、输出参数及关联文件；`TaskRecord` 含歌曲 ID、可选歌曲详情、选项快照、各输出的 `OutputState` 和任务阶段。`OutputState` 区分 Pending、Saved、Skipped、Unavailable、Failed；`TaskStage` 使用设计文档中的等待至中断阶段。`DownloadEvent` 携带任务快照、文件索引变化或位置失效事件，不携带网络签名 URL。

---

### Task 1: 输出模型与命名规则

**Files:** 新增 `src/lib.rs`、`src/download/{mod,model,naming}.rs`、`tests/download_naming.rs`、`tests/common/mod.rs`；修改 `Cargo.toml` 和 `src/main.rs`。

**Interfaces:** 产出共用类型；`sanitize_component(input: &str) -> String`；`song_basename(song: &SongInfo, ordinal: Option<(usize, usize)>) -> String`；`DownloadOptions::validate(&self) -> Result<()>`。外部效果不依赖 GTK 初始化。

- [x] **Step 1: 写失败测试**：`sanitizes_path_and_reserved_names` 验证 `../CON:歌/曲` 不含分隔符 / 控制字符、不会产生保留名；`truncates_unicode_at_character_boundary` 验证有效 UTF-8 且不超过 180 字节；`numbers_from_snapshot` 验证 1/1000 前缀为 `0001 `；`rejects_empty_lyric_selection` 验证仅歌词且无格式返回错误。
- [x] **Step 2: 运行 RED**：`cargo test --offline --test download_naming`；预期因尚不存在命名函数 / 类型失败。
- [x] **Step 3: 实现**：定义上述序列化类型、参数验证、净化与编号；只为未来已确定的消费者增加字段。测试工具使用带唯一名的 `/tmp` 子目录并在 Drop 清理自己的文件。
- [x] **Step 4: 运行 GREEN**：同一命令及 `cargo test --offline`；预期所有命名测试及项目原有测试通过。
- [x] **Step 5: 提交**：仅提交本任务源码、测试和必要 Cargo 变动，消息 `feat: define download outputs and portable filenames`。

### Task 2: 歌词解析与多格式导出

**Files:** 新增 `src/download/lyrics.rs`、`tests/download_lyrics.rs`；修改模块导出和 `Cargo.toml`，直接依赖 `encoding_rs = "0.8"`。

**Interfaces:** `LyricDocument::parse(original: &[String], translated: &[String]) -> LyricDocument`；`render(&self, format: LyricFormat, mode: LyricMode, encoding: TextEncoding, duration_ms: Option<u64>) -> Result<Vec<LyricOutput>>`。`LyricOutput` 含文件后缀和完整文本字节，由 model 定义；storage 消费，不由歌词模块写盘。

- [x] **Step 1: 写失败测试**：多时间标签展开为 1000 / 2000 ms；异常 `[00:01:20]` 归一为 `[00:01.20]`；相同时间译文对齐；TXT 去标签；无时间标签时 TXT 可用、LRC / SRT 不可用；最后字幕在无时长时结束于起点 + 3000 ms；GBK 导出含 emoji 时返回编码错误。
- [x] **Step 2: 运行 RED**：`cargo test --offline --test download_lyrics`，预期缺少歌词实现失败。
- [x] **Step 3: 实现**：支持 LRC 标准 / 异常标签、稳定排序、多标签、元数据与正文区分；SRT 同起点合并、非正长度过滤；UTF-8 无 BOM / CRLF，GBK 严格编码；独立译文使用 `.translated` 后缀。
- [x] **Step 4: 运行 GREEN**：歌词测试及完整 `cargo test --offline`；预期通过，测试使用手算时间和字节，不调用待测函数生成预期值。
- [x] **Step 5: 提交**：`feat: export original and translated lyrics as LRC TXT and SRT`。

### Task 3: 完整歌单与可取消音频请求

**Files:** 新增 `vendor/netease-cloud-music-api/`（当前 2.2.0 的库源码、Cargo 清单、许可证、来源说明）、`src/download/api.rs`、`tests/download_api.rs`；修改 `Cargo.toml` 的本地 `[patch]` 配置，保留现有 git tag。

**Interfaces:** 依赖 model。补丁新增 `DownloadResource { id, url, codec, expected_duration_ms, preview: bool, source_rate }`、`PlaylistTracks { id, name, track_ids, total }`；`MusicApi::download_resource(id: u64, bitrate: u32) -> Result<DownloadResource>`、`playlist_tracks(id: u64) -> Result<PlaylistTracks>`、`download_to(url: &str, path: &Path, cancelled: &AtomicBool, progress: impl Fn(u64, Option<u64>)) -> Result<()>`。adapter 产出 `complete_collection(client: &MusicApi, id: u64) -> Result<CollectionSnapshot>` 和无副作用 `fetch_lyrics(client: &MusicApi, id: u64) -> Result<LyricDocument>`。

- [x] **Step 1: 写失败测试**：原始响应中的试听标记保持可见；空 URL 拒绝；1001 个 track IDs 分批获得全部详情；重复 ID 保留首次顺序且先核对总数；缺失详情形成失败项；本地 HTTP 404 / 截断 / 中途取消不能成功，取消后不继续增长目标文件。
- [x] **Step 2: 运行 RED**：`cargo test --offline --test download_api`，预期缺少新接口失败；响应解析测试使用手写完整 JSON，流式行为测试使用临时本地 TCP 服务。
- [x] **Step 3: 实现**：复用依赖现有认证 / 代理 / 加密，保留原播放接口；解析完整曲目 ID、试听字段及预期时长；通过响应流读写固定大小缓冲区，每次检查取消、统计进度和失败状态；不使用原有整文件缓冲下载方法。
- [x] **Step 4: 运行 GREEN**：API 测试及完整项目测试；构建仍使用本地已安装依赖；预期无实际网易请求，2.2.0 原接口兼容。
- [x] **Step 5: 提交**：`feat: add complete playlist and validated streaming download APIs`；不提交用户 cookie、日志或签名 URL。

### Task 4: 目录身份、文件索引、安全保存与删除

**Files:** 新增 `src/download/{storage,catalog}.rs`、`tests/download_storage.rs`；添加 `rustix = { version = "1", features = ["fs"] }`。

**Interfaces:** `TargetLocation::capture(root: &Path, removable: bool, volume_uuid: Option<String>, mount_session: Option<String>) -> Result<Self>`；`validate_location(&TargetLocation) -> Result<()>`；`commit_file(prepared: &Path, target: &TargetLocation, relative: &Path, cancelled: &AtomicBool) -> Result<StoredFile>`；`scan(target: &TargetLocation) -> Result<Vec<SavedSong>>`；`preview_delete(items: &[SavedSong], include_audio: bool, include_lyrics: bool) -> Result<DeletePreview>`；`execute_delete(preview: &DeletePreview, mode: DeleteMode) -> DeleteReport`；`Catalog::load(path: &Path) -> Result<Catalog>` / `save(&self, path: &Path) -> Result<()>`。model 定义预览 / 报告和回收站 / 永久两种模式。

- [x] **Step 1: 写失败测试**：同名目标保留旧字节；路径逃逸和中间符号链接拒绝；复制取消不生成正式文件；身份变更后不得重建目标目录；扫描不跟随链接；清除历史保留索引与待清理清单；同大小同时间的替换文件导致删除预览失效；另一目录副本保留；回收站失败不得执行永久删除；部分删除报告逐文件结果。
- [x] **Step 2: 运行 RED**：`cargo test --offline --test download_storage`，预期缺少存储实现失败。回收站测试用明确失败的受控边界，真实永久删除只操作独立临时目录。
- [x] **Step 3: 实现**：固定目录身份、根目录句柄、逐级拒绝链接；流式复制到本任务临时文件，sync 后使用 Linux 不覆盖 rename 提交；内容 SHA-256 指纹以 GLib 校验和流式计算；JSON 版本化替换并保留可读旧版；扫描普通 MP3 并展示待纳入关联；删除前重新核对身份 / 路径 / 指纹，永久删除使用根目录相对文件操作，回收站使用安全句柄移动并写 Freedesktop Trash 信息，失败不回退（审阅修正）。
- [x] **Step 4: 运行 GREEN**：存储测试及完整项目测试；预期正式文件、历史和清理清单均符合设计，没有越界副作用。
- [x] **Step 5: 提交**：`feat: safely save index scan and delete downloaded files`。

### Task 5: MP3 转换、媒体验证和标签

**Files:** 新增 `src/download/audio.rs`、`tests/download_audio.rs`；添加 `id3 = "1"` 直接依赖。

**Interfaces:** `probe(path: &Path) -> Result<AudioInfo>`；`verify_complete(actual_ms: u64, expected_ms: u64, preview: bool) -> Result<()>`；`prepare_mp3(input: &Path, output: &Path, song: &SongInfo, bitrate: u32, ordinal: Option<usize>, cancelled: &AtomicBool, progress: impl Fn(f64)) -> Result<AudioInfo>`；`check_plugins() -> Result<()>`。`AudioInfo` 为编码、时长、采样率、声道、码率的独立值；返回前验证全部输出约束。

- [x] **Step 1: 写失败测试**：真实生成的短 AAC / FLAC / MP3 夹具输出 MPEG Layer III、44100 Hz、2 声道、192000 bps；读取标签版本为 2.3 且歌名正确；合法 10 秒短歌通过，明确 preview 拒绝；100 秒源比预期少 10 秒拒绝；取消时释放管线且不提交输出；符合约束的 MP3 不重复编码。
- [x] **Step 2: 运行 RED**：`cargo test --offline --test download_audio -- --test-threads=1`，预期缺少转换实现失败；夹具由测试内 GStreamer 音源生成，不依赖下载版权音频。
- [x] **Step 3: 实现**：独立媒体管线和总线处理，非 GTK 主线程探测和转换；控制采样率 / 声道 / CBR；符合目标时复制 MP3 帧；id3 写基本 v2.3 标签；以 `max(2000 ms, expected_ms / 100)` 验证时长容差；释放时切到 Null；详细错误不含资源 URL。
- [x] **Step 4: 运行 GREEN**：音频测试及完整项目测试；预期通过。如果机器缺少样本编码插件，明确记录环境限制，不能把未执行媒体测试当作通过。
- [x] **Step 5: 提交**：`feat: produce verified constant bitrate MP3 downloads`。

### Task 6: 应用级下载队列和输出重试

**Files:** 新增 `src/download/{manager,worker}.rs`、`tests/download_manager.rs`；添加 `futures-lite = "2"`；修改 `src/download/mod.rs`。

**Interfaces:** 消费 Tasks 1–5。`DownloadManager::new(data_dir: PathBuf, temp_dir: PathBuf) -> Result<(DownloadManager, Receiver<DownloadEvent>)>`；`enqueue(&self, songs: Vec<SongInfo>, options: DownloadOptions, collection: Option<CollectionSnapshot>, client: MusicApi) -> Result<Vec<TaskId>>`；`enqueue_collection(&self, collection: CollectionSnapshot, options: DownloadOptions, client: MusicApi) -> Result<Vec<TaskId>>`；`cancel(&self, id: TaskId)`；`retry(&self, id: TaskId, client: MusicApi) -> Result<()>`；`invalidate_location(&self, id: &LocationId)`；`stop_network_tasks(&self)`；`clear_history(&self) -> Result<()>`；`snapshot(&self) -> ManagerSnapshot`；`shutdown(&self)`。enqueue_collection 对缺失详情的条目生成明确失败记录，对有效条目沿用 enqueue；ManagerSnapshot 包含任务、位置和 SavedSong；公开值类型均由 model 定义。

- [x] **Step 1: 写失败测试**：同一任务连续入队去重，不同位置允许副本；两个下载和一个编码上限；歌词失败重试保留音频字节 / 指纹；取消保留已提交文件；重启活动任务转 Interrupted；清除历史保留文件索引；位置失效停止正在复制和排队任务；账号退出阻止新网络步骤；进度事件节流但最终完成必达。
- [x] **Step 2: 运行 RED**：`cargo test --offline --test download_manager`，预期缺少管理器失败。用本地资源服务和真实临时目录，慢操作边界注入受控执行器，断言生产队列状态 / 文件，不断言假对象存在。
- [x] **Step 3: 实现**：后台工作线程驱动异步 API 和同步管线；调度最多两个任务，单一编码许可；任务快照去重、文件结果驱动只重试缺失项；按阶段持久化和发布事件；保留离线临时清理清单；主线程不等线程 join，应用退出阶段协调关闭。
- [x] **Step 4: 运行 GREEN**：队列测试及完整项目测试；预期所有任务终态与实际文件一致，网络重试重新获取链接。
- [x] **Step 5: 提交**：`feat: manage cancellable batch downloads and partial retries`。

### Task 7: 下载对话框和首选项

**Files:** 新增 `src/gui/download_dialog.rs`、`data/gtk/download-dialog.ui`；修改 `src/gui/{mod,preferences}.rs`、`data/gtk/preferences.ui`、GSettings schema、gresource、`src/meson.build`；新增 `tests/download_ui.rs`。

**Interfaces:** `DownloadDialog::new(initial: DownloadPreferences, count: usize, collection: Option<CollectionSnapshot>) -> Self`；`connect_submit(&self, callback: impl Fn(DownloadOptions) + 'static)`；`present_for(&self, parent: &impl IsA<gtk::Widget>)`。纯参数读取 / 验证复用 model，用户提交后通过后台存储模块确认目录，完成捕获才发送 DownloadOptions；不在模板回调中执行 IO。

- [x] **Step 1: 写失败测试**：schema 默认值产生 192 kbps / 原文 LRC / UTF-8；仅歌词无格式阻止提交；仅歌词隐藏码率；更改选项提交正确快照且不修改已入队参数；对话框可在 Libadwaita 1.5 实际实例化。
- [x] **Step 2: 运行 RED**：`cargo test --offline --test download_ui -- --test-threads=1` 在可用显示 / 虚拟显示环境下运行；预期缺少新模板或控件失败；纯选项验证不依赖显示。
- [x] **Step 3: 实现**：现有 Adw 分组行、GtkFileDialog 文件夹选择器、可用空间显示；schema 增加下载默认项；原生按钮和提示，无设备型号要求。更新资源和 Meson 文件列表，使新增模板可被测试和安装加载。
- [x] **Step 4: 运行 GREEN**：实际 UI 测试、`glib-compile-schemas --strict --dry-run data` 和完整测试；预期可实例化、无 schema 错误，既有首选项绑定保留。
- [x] **Step 5: 提交**：`feat: add native download settings and preferences`。

### Task 8: 下载入口、选择模式与管理页面

**Files:** 新增 `src/gui/downloads_page.rs`、`data/gtk/downloads-page.ui`；修改 `src/{application,window,main}.rs`、`src/gui/{songlist_row,songlist_view,songlist_page,search_song_page,player_controls,mod}.rs` 和对应模板、window 主菜单、资源和 Meson 列表；扩展 `tests/download_ui.rs`。

**Interfaces:** `DownloadsPage::new(manager: DownloadManager) -> Self`；`apply_event(&self, event: DownloadEvent)`。Action 新增 `OpenDownloads`、`RequestDownload(Vec<SongInfo>, Option<CollectionSnapshot>)`、`RequestCollectionDownload(u64)`；应用持有唯一 manager 并监听 events，打开页面不创建新队列。管理页文件动作复用 Task 4 预览 / 执行，不自行拼接路径。

- [x] **Step 1: 写失败测试**：单曲下载按钮不触发行激活；多选行点击只切换选中、不播放；退出选择模式恢复播放；搜索全选只针对已加载歌曲且显示范围；关闭管理页队列继续；回收站失败显示保留项且不隐式永久删除；文件永久删除必须经过包含目录 / 文件清单的确认。
- [x] **Step 2: 运行 RED**：显示环境下运行 UI 测试；预期新入口 / 模式缺失失败。测试发出真实控件事件并记录实际 Action 消息。
- [x] **Step 3: 实现**：行下载、播放栏入口、歌单 / 专辑下载全部、多选；主菜单管理页接现有 PageStack；任务 / 已保存两个视图、文件扫描纳入预览、搜索与删除确认；监听挂载失效、登录退出和应用退出；使用现有 Toast 汇总，不逐项弹通知。
- [x] **Step 4: 运行 GREEN**：UI 测试和完整测试；在现有窗口尺寸及较窄尺寸检查主题 / 布局、键盘和实际模板回调；验证页面跳转、播放和选择模式之间没有冲突。
- [x] **Step 5: 提交**：`feat: integrate batch download and saved file management UI`。

### Task 9: 翻译、安装资源和整体验收

**Files:** 修改 `po/POTFILES.in` 和中文 PO、`README.md`、`src/meson.build` / gresource（若需要）；新增 `docs/song-download-usage.md`；更新本计划已完成项。

**Interfaces:** 不增加新的业务 API；完成全部功能的用户可见入口、安装资源和错误提示。

- [x] **Step 1: 写 / 扩展端到端失败测试**：本地服务提供 2 首不同源编码歌曲和原文 / 译文，批量下载到临时目标，检查 MP3 / LRC / TXT / SRT、索引和记录一致；删除其中一首保留另一首及另一目录副本；音频成功而歌词缺失显示部分结果。
- [x] **Step 2: 运行 RED**：运行该端到端测试，预期暴露尚未连通的流程；如果已经通过，测试已有集成行为，不制造虚假 RED，不新增无用实现。
- [x] **Step 3: 补齐**：修复端到端暴露的实际问题；新增所有文案和中文翻译、插件需求说明、保存 / 删除说明；不在 UI 暴露内部实现字段。
- [x] **Step 4: 整体验证**：`cargo fmt --check`、`cargo test --offline`、`cargo clippy --offline --all-targets`、`meson compile -C _build`、schema 和 PO 检查；实际启动检查浅 / 深色、小窗口、播放不中断、文件夹选择、下载 / 删除预览。TF 卡保存验证需要实际可用设备，缺失时明确报告限制，不自动删除真实用户数据。
- [x] **Step 5: 提交并审阅**：提交代码 / 测试 / 文档；按 Superpowers requesting-code-review 做一次全量独立审阅，重要问题以 RED→GREEN 修复；保留可审阅提交，不自动推送或发布。确认格式、构建、测试和实际 UI 验证结果后报告完成。

## 执行建议与审阅状态

建议 Native：同一会话按任务顺序实现，最后进行一次独立审阅。任务共享模型、文件索引和管理器接口，顺序实现可以减少多名实现者同时修改同一文件的冲突。实现期间按计划记录进展，不在每个任务之间重复请求继续授权。

用户已选择 Native 执行：当前 checkout 的 `feature/song-download` 分支。原有 adw v1_5 修改保留为未暂存改动；功能提交不包含这项用户修改。

编写计划期间已经运行原项目 `cargo test --offline`：编译成功，当前项目没有测试（0 passed / 0 failed）。后续验证不得把这次零测试基线当成新功能的测试证据。本机未找到 Xvfb / xvfb-run；UI 测试先检查实际显示环境，无法使用时需要安装虚拟显示依赖的授权或明确报告尚未完成的 UI 验证。
