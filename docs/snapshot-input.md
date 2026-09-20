# 本地官方文件输入清单（#74）

`inspect_snapshot <manifest.json> <trusted-input-directory>`只流式核验本地文件字节，不下载、不解压、不发布、不修改输入。manifest限制1MiB（工程防误用限制，非性能承诺），每个文件以64KiB缓冲读取。

schema为`x-data-mcp.official-file-input.v1`，files必须恰有三个不同role：`base_industry`、`day`、`gbbq`。每项包含relative_path、source_url、acquisition_started_at/completed_at（带时区时间）、bytes、sha256（小写64位），可选last_modified/etag。来源URL是声明的TDX域名HTTP(S)地址，仅检查形式；没有证明下载者确实从该地址取得字节。清单不接受额外字段或生产批准开关。

三个角色分别指官方base/行业资料、日线资料、股本变迁资料的具体输入文件。当前可以是保留的档案包；本检查不验证zip成员归属或其业务内容，不将任意同名文件当成已验证格式。需要分别保留归档和成员关系证据，后续解析器验证负责内容。

获取区间各自保留，倒置/未来区间拒绝。相同下载日、Last-Modified、ETag或hash匹配均不能建立.day/gbbq修订兼容性。输出同时保留清单原始字节hash、逐文件元数据及FILE_BYTES_VERIFIED，明确origin NOT_AUTHENTICATED、archive_member_binding/file_set_consistency/business_semantics/access_use NOT_VERIFIED，production_ready=false。

路径必须为输入目录内相对路径，拒绝父级/绝对路径、缺失文件以及解析到目录外的软链接。要求输入目录私有、可信且核验期间不变；不是对恶意进程并发替换目录的安全沙箱。摘要与长度不匹配时失败，CLI退出非零且不输出成功JSON。

这为未来文件获取阶段与SQLite元数据存储提供输入契约，不连接OfficialFilePipeline。Inspection 输出的各 NOT_VERIFIED 维度表示"本次字节核验本身不证明该维度"；门禁级结论由 FG 裁定与提交证据承载——#54/#71/跨文件一致性已按 FG 裁定解决（见 source-closure.md），#6/#46 目录语义仍缺。Live source NOT_RUN，现有test使用人工字节fixture，只证明完整性与拒绝行为。
