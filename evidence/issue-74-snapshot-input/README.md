# Retained输入完整性验证

仅读取既有三个归档包，无网络下载、无全市场记录解析。manifest的URL、取得区间、长度、hash、Last-Modified及ETag来自既有#46/#6 DBF证据和#3 hsjday证据；没有改写旧文件。

本环境输入根目录为/tmp：tdx-dbf-evidence.zXosOa/base.zip、tdx-dbf-evidence.zXosOa/gbbq.zip、tdx-vipdata-evidence.qusdV2/hsjday.zip。新环境必须先获得完全相同的保留字节，不能静默下载当前版本替代。

运行：

```sh
cargo run --locked --bin inspect_snapshot -- evidence/issue-74-snapshot-input/retained-manifest.json /tmp
```

实际结果在result.json：三文件匹配，但origin、成员关联、跨文件一致性、业务语义和使用权限均未验证，production_ready=false。文件角色由清单声明，不是解析器证明归档业务范围完整。两个不同修改时间及独立获取区间没有被解释为同快照。

输出不含运行时钟，固定输入和可接受的检查时点产生稳定字节。输入日期仍必须不晚于本次检查时点。CI人工字节fixture只证明核验/拒绝，外部归档包不在CI中；live source NOT_RUN。
