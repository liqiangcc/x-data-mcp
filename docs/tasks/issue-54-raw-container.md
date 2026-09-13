# Issue #54：GBBQ 原始容器切片

本切片只校验保留证据中观察到的 4 字节 little-endian 记录数和 29 字节记录宽度，并逐字节保留加密记录。29 字节布局来自社区 decoder evidence，不能当作通达信官方语义契约。

解析器不解密、不解释 category、C1/C2/C3/C4，不进行 float32 到业务十进制转换，不计算除权因子或 QFQ，也不接入 `OfficialFilePipeline`。因此不会以社区算法替代官方证据。

测试使用合成字节，只证明长度校验与保真；#54 的 normalization contract、半分边界、事件映射、`.day`/`gbbq` 一致性和授权仍 `NOT_VERIFIED`，真实来源与 #53 四股复验仍 `NOT_RUN`。
