# .day 原始解析（#64）

Rust `day_file::parse`按已有hsjday结构证据读取固定32字节little-endian记录。
仅转换可验证的日历日期；四个OHLC uint32、amount float32位模式、volume uint32和第8字段
(`r7_raw`)全部保留。零值和非有限float位模式不静默过滤。volume（股）/amount（元）语义经
#239 TdxQuant oracle锚定后晋升入发布bar，原值直存、不经QFQ调整；`r7_raw`仅以原始u32
入库（#240：A股1999–2010为官方除权基准价fen，基金/指数语义未锚定，2010-09后官方退化为
0x10000），不进查询契约。
截断、无效日期、重复/倒序会拒绝。空文件可结构解析为空，**不表示覆盖完整或可发布**。

`cargo run --locked --bin inspect_day -- /path/to/sh600006.day`只读取显式本地文件，输出
SHA256、字节数、记录数和首末日期。无网络、无发布、无Python依赖、无十进制规范化或QFQ。

有界retained验证重用#53四个已选官方成员，比较官方保留hash、记录数和日期范围。该验证说明
Rust解析与既有结构证据一致，不把社区逆向格式提升为官方业务契约。文件快照一致性、目录及
获取使用授权仍未完成。OfficialFilePipeline门禁未变。
