# Issue #46：官方目录文件解析基础

本切片只加入保守的 DBF 容器解析。解析器校验头部、字段描述符、记录边界和删除标记，并逐字节保留字段值；它不解释代码页、字段业务含义、沪深身份范围或研究行业层级。

因此该切片不能关闭 #46/#6 的目录契约门禁，也不接入 `OfficialFilePipeline`。`base.dbf` 的保留证据显示代码页 `0x7a`，但代码页语义仍未知；解析器会把它作为元数据返回，不据此把原始字节解码成名称或身份。

测试使用合成 DBF，仅证明容器损坏、截断、删除记录和原始字段字节不会被静默修复或丢弃。真实官方文件解析、成员绑定、完整覆盖、跨文件一致性、授权和部署仍为 `NOT_VERIFIED` / `NOT_RUN`。

## 验收映射

- 容器结构：`tests/dbf.rs::preserves_header_fields_deletion_and_raw_field_bytes`。
- 损坏拒绝：`tests/dbf.rs::rejects_truncated_or_malformed_containers_without_guessing`。
- 业务契约：未实现，保持 `NOT_VERIFIED`，不由合成 fixture 推断。
