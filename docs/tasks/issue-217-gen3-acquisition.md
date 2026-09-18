# Issue #217——gen-3 官方快照采集规格(Windows 执行)

一次有界采集,同时供给三条阻塞链:changed-generation update 验收、FILE_SET_CONSISTENCY 取证、#54 normalization oracle。**采集动作全部在 Windows 侧执行**;本机负责规格、接收、登记与后续验收。

**执行入口**:`scripts/gen3_acquire.py`(stdlib only)——`python gen3_acquire.py --out gen3-run-<date>` 一次完成 §1 下载+清单、§2 TQ oracle(`issue54.tq-oracle.v1`)、§3 元数据、§4 #71 模板;`--skip-download`/`--skip-tq` 可分步,不可得的项如实记 `NOT_RUN`。

## 0. 原则

- 沿用 gen-2 纪律:官方文件**只下载一次**,字节先入 `retained-inputs/` 登记新代,再用于证据;gen-2 不覆盖、不删除;
- 每条采集记录**获取事实**(UTC 起止、URL、HTTP 状态、Last-Modified、ETag、bytes、sha256)——取得过程本身就是证据的一部分;
- oracle 输出**逐字留存**(API 原始返回原样落盘,不清洗、不筛选),判别规则在拿到数据前冻结;
- 拿不到的项**如实记 NOT_RUN/insufficient**,不编造 `CompatibilityEvidence`,不把辅助元数据当兼容证明。

## 1. 第二部分:二代文件集(changed-update + 一致性输入)

| 角色 | gen-2 参照 | 动作 |
|---|---|---|
| gbbq | `official-gbbq.zip` sha256 `daad6290…d0e9`(6358077B,2026-09-15,Last-Modified `Mon, 14 Sep 2026 10:10:10 GMT`,ETag `"6aa7c802-61043d"`) | 同一 URL `https://www.tdx.com.cn/products/data/data/dbf/gbbq.zip` 重新下载 |
| hsjday | `official-hsjday.zip` sha256 `c82f852a…0892`(~549MB) | 同 gen-2 渠道重新下载 |
| base | `official-base.zip` sha256 `dcdf6b49…8bc6`(含 tdxhy.cfg/base.dbf/infoharbor_ex.*) | 同 gen-2 渠道重新下载 |

每文件落盘后立即记:

```json
{"role":"gbbq|hsjday|base","url":"...","retrieved_utc":"...","http_status":200,
 "last_modified":"...","etag":"...","bytes":N,"sha256":"..."}
```

并解出 gbbq member 另行登记(gen-2 member `4a3e9b86…9d77`,205776 条)。

**预期事实**(用于验收设计,非断言):gbbq 为滚动文件,gen-3 应含新权息事件;hsjday 应有新交易日 bar;两代的同一文件大概率 hash 不同——这正是 changed-update 验收要的输入。

## 2. 第三部分:#54 normalization oracle

### 背景

`gbbq` member 每事件含 `category` + 四个 float32(C1–C4)。五个规范化候选在九个冻结事件上**全部自洽**(能解释 600006 半分边界),无法区分;官方 `get_divid_factors` 文档已复核,**只定义 API 字段语义,不定义二进制映射**——文档路径穷尽,唯一剩余裁决路径是官方 oracle 实测值。

### oracle 手段(按可得性排序)

1. **TdxQuant(TQ API)**——Windows 上 `tqcenter` 环境(官方量化平台,随客户端提供):
   ```python
   tq.get_divid_factors(stock_code=CODE, start_time='', end_time='')
   tq.get_market_data(field_list=[], stock_list=[CODE], period='1d',
                      start_time='', end_time='', count=0,
                      dividend_type='none', fill_data=False)
   tq.get_market_data(...同上..., dividend_type='front')
   ```
2. **客户端前复权显示**——无 TQ 时的降级:官方客户端切"前复权",对指定证券+指定事件窗口截图/抄录 OHLC(逐日记日期与四价,注明"前复权"档)。

### 冻结采集集(已从 gen-2 retained member 机械生成)

| 层 | 证券 | 携带的关键事件 |
|---|---|---|
| #53 回归四股 | sh.600004, sh.600006, sz.000002, sz.000004 | sz.000004 @19930524 cat1 **四字段全非零**(0.8/10.0/3.0/4.0);sz.000002 @19910601 cat1 C2&C4 非零(4.4/5.0);sh.600006 @20040611 ShareBonus 10.0;sh.600004 多 cat1 红利事件 |
| 官方文档样本 | sh.688318 | `get_divid_factors` 官方示例证券;**已免费交叉验证**:官方示例 2020-09-29 Type=1 Bonus=6.0 ↔ member (20200929,cat1,v5=6.0) 逐字一致——v5=Bonus、v7=ShareBonus 在该样本上已由官方文档佐证 |
| 全字段判别股 | sz.000001 | 仅 23 条 cat1 全四字段非零事件之一,且一股含两条(19930524:3.0/16.0/8.5/1.0;19940711:5.0/5.0/5.0/1.0) |
| cat-15 代表 | sz.000035 | `Type=15 重新调整` 事件 @20121130(101 条 cat15 中按序首只 A 股) |

机械事实(205776 条 member):category 分布 {1:68669, 5:116312, 2:4865, 3:4920, 9:7977, 11:1805, 10:304, 15:101, 余<100};cat1 全四字段非零仅 23 条(典型 float32 存储痕迹 0.800000011920929/7.949999809265137,正是"float32→业务十进制"问题本身);cat11 集中于 sz.15xxxx 分级基金,非 A 股主线。

判别设计的核心:**同一事件多字段同现**迫使候选规则在"字段↔官方语义 + scale + 舍入"上同时自洽——sz.000004 的 0.8/10.0/3.0/4.0 与 sz.000001 的 3.0/16.0/8.5/1.0 若 TQ 给出官方逐字段值,五候选里能同时满足多字段映射与 QFQ 结果的应当至多一条。

### 每股采集内容

- `get_divid_factors` 全历史 → 逐字 CSV/repr;
- `get_market_data` `dividend_type=none` 全历史 → 不复权 OHLC + `ForwardFactor` 逐字;
- `get_market_data` `dividend_type=front` 全历史 → 官方前复权 OHLC 逐字;
- 每次调用记:调用时刻 UTC、API/客户端版本、原始返回字节。

### 判别设计(拿到数据后执行,规则此处预冻结)

1. 对每个冻结事件:(a) 用 gen-2 gbbq 字节 + `.day` 前收盘按候选规则 i 预测官方语义字段与 QFQ 价;(b) 与 TQ `get_divid_factors` 字段 + `front` 输出逐值比对;
2. **事前冻结**:比对字段、精度口径(价格分、比例精度)、判定为"命中"的容差定义(精确等值优先;float32 尾差规则先写死再比);
3. 唯一命中候选 → 提名 normalization contract,回 #54 走"事前冻结规则 + 九事件矩阵 + #53 四股"验收;多候选命中 → 扩判别集(更多 C2/C4 同现事件);零命中 → 规则集合有误,如实记录重立项;
4. `get_divid_factors` 的 `Type` 枚举(1 除权除息/11 扩缩股/15 重新调整)同时校验 `category` 语义映射。

## 3. 第四部分:一致性元数据(FILE_SET_CONSISTENCY 取证)

validator 只接受绑定三角色 sha256 + set identity/revision + 时间窗的显式证明。采集时留存**一切**可审计的集合元数据:

- 三个下载的响应头(Last-Modified/ETag)——辅助证据,**明确不构成**兼容证明;
- 官方下载页/vipdata 页快照(若页面声明文件集版本、更新时点、配套关系);
- 渠道暴露的任何版本号/更新清单/修订说明;
- gen-2 与 gen-3 的关系事实(哪些成员新增/不变)——跨代一致性由两代 hash/计数/内容对比支撑。

**如实预期**:官方公开渠道大概率不给出版本化兼容声明 → `CompatibilityEvidence` 可能仍构造不出,此时记录"证据不存在于可得渠道",blocker 保持 NOT_VERIFIED,不降格为推断。

## 4. 第五部分:#71 使用边界(文字裁定,非采集)

请书面给出:使用主体(本服务/个人)、覆盖文件角色、获取频率上限、留存期限、解析计算与 AI 访问范围——对应 `FILE_ACCESS_USE_NOT_VERIFIED` 的裁定输入。

## 5. 交付与登记

- gen-3 文件与 oracle 输出放 `retained-inputs/` 新代目录,`MANIFEST.md` 追加 gen-3 段(逐文件 sha256 + 获取事实);原始字节不入库,派生证据入库;
- 本机收到后按序执行:gen-3 完整性核验 → changed-update 验收 PLAN → #54 候选判别 → 一致性 validator 实跑 → 各自 FG 裁定。

## 6. 边界

- 不推定官方授权扩大;oracle 仅作**比对参照**,不引入为运行依赖(生产 pipeline 仍只吃本地文件);
- oracle 是**语义裁决**手段,不是第二数据源——不引入 fallback、不做逐股修补;
- gen-3 若与 gen-2 完全相同(无新事件/bar),changed-update 验收降级为"同代重复"(已知 NoChange 语义),如实记录;
- 本规格不构成对官方渠道的爬取许可;一切采集限 gen-2 已授权的同类公开渠道。
