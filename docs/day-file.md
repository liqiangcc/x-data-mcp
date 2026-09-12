# .day 原始解析（#64）

Rust `day_file::parse`按已有hsjday结构证据读取固定32字节little-endian记录。
仅转换可验证的日历日期；四个OHLC uint32、amount float32位模式、volume uint32和reserved
全部保留。零值和非有限float位模式不静默过滤；业务层以后必须单独验证它们，不能直接发布。
截断、无效日期、重复/倒序会拒绝。空文件可结构解析为空，**不表示覆盖完整或可发布**。

`cargo run --locked --bin inspect_day -- /path/to/sh600006.day`只读取显式本地文件，输出
SHA256、字节数、记录数和首末日期。无网络、无发布、无Python依赖、无十进制规范化或QFQ。

有界retained验证重用#53四个已选官方成员，比较官方保留hash、记录数和日期范围。该验证说明
Rust解析与既有结构证据一致，不把社区逆向格式提升为官方业务契约。量纲、amount浮点到业务
十进制、GBBQ、QFQ、文件快照一致性、目录及获取使用授权仍未完成。OfficialFilePipeline门禁未变。
