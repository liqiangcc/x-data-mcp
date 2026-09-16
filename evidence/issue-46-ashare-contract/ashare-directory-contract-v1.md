# 沪深A 目录组合契约 v1（issue-46 草案）

refs #46, #6。本契约由 gen-2 retained 官方文件机械验证支撑（`scan_ashare_contract.py` → `result.json`，20/20 checks ok）。它声明“本代文件可被证明的事实”，不声明交易所官方完整性或生产授权。

## 1. 输入与来源标识

| 输入 | 身份 | 用途 |
|---|---|---|
| `official-base.zip` (sha `dcdf6b49…`) | gen-2 retained，授权 #71 comment-5676627357 | 身份/名称/范围/归属成员容器 |
| `official-base.zip!tdxhy.cfg` (sha `df7cb705…`) | 5659 行，与 #167 Windows V7.73 live 拷贝字节一致 | 范围 universe + f0/f2/f5 |
| `official-base.zip!infoharbor_ex.code` (sha `79eed0f6…`) | 5571 行 `code\|name\|keywords` | 名称来源 |
| `official-base.zip!base.dbf` / `base.map` | 8875/8875 行，GPDM 码字段 | 机械覆盖佐证（语义 UNANCHORED） |
| `official-gbbq.zip!zhb.zip!incon.dat` (sha `86f758b2…`) | #TDXNHY 145 条 / #TDXRSHY 474 条，与 Windows 拷贝字节一致 | T/X 码→名称字典 |
| `binding.json` (#164, sha `67508fca…`) | PDF 声明层级 + 三方绑定矩阵 | 层级断言依据 |

## 2. 已锚定语义（#164 + #167 + FG ruling comment-5690659606）

- `tdxhy.cfg f0` = 交易所枚举 `{0:SZ, 1:SH, 2:BJ}`；`f1` = 6 位证券代码（保前导零）；`f2` = TDX 行业 T-code；`f5` = TDX 研究行业 X-code leaf；`f3/f4` 本代恒空，不赋语义。
- X 码层级：`(len(code)-1)//2`（L1=`X··`、L2=`X····`、L3=`X······`），由官方 PDF 表声明并由 #164 binding 逐码复核。
- 证券 X 二级归属 = `f5[:5]`，本代 5230/5230 S 成员该前缀均在 TDXRSHY 且 level==2（机械断言，非推断）。
- 退市缺席已锚定：000004 不在 tdxhy.cfg/ex.code，存在于 tdxhy2.cfg/base.dbf（扩展/历史面）。

## 3. 组合契约

**范围 S（沪深A 候选）**：`f0 ∈ {0,1} 且 f2 非空 且 f5 非空` → 5230 成员。
排除集必须显式枚举，不得静默丢弃：BJ `f0=2`（348 行）；空 T/X 非 A 行 81 条 = B 股 `200xxx/900xxx` 78 条 + 异常码 `002710/002720/201872` 3 条（A 码段但无行业亦无名称，如实列出）。

**身份**：`(f0, f1)` = （交易所， 6 位代码）；代码不剥离前导零。来源标识 = 上表 member sha。

**名称**：`infoharbor_ex.code` f0→f1（GB18030）。覆盖 5224/5230；缺名成员 `001235, 300728, 601206, 603302, 603361, 688688`（base.zip 全部成员内均无名称行）→ 按 AC-04 如实报告；缺名成员的处置（拒绝发布 vs 标记缺名）属 #6 待裁定项，本契约不擅自放行。`infoharbor_ex.name` 非 A 股名称文件（仅 304/3245 行落在 S；f0 枚举与 f0 不同），角色 UNANCHORED。

**研究行业二级**：成员 → `f5[:5]`（L2 码）→ `TDXRSHY` 名称；层级链 `f5[:3]`→L1、`f5[:5]`→L2、`f5`→leaf 逐成员验证存在且 level 正确。T-code（f2）为并行旧分类，仅 `688688` 的 `T00` 不在 TDXNHY（退化值，如实报告）。

**快照/保护（沿用 #6 已确认规则）**：单归档成员共享一次下载快照；成员 sha 逐文件钉死，跨代漂移（gen-1/gen-2 成员 hash 不同，见 #161 findings）必须重新 pin；候选快照非空、成员数=记录数、无重复身份才可发布；成员消失 → 整候选拒绝（AC-08）；刷新失败/不完整 → 保旧（AC-04）；首次失败 → 未就绪（AC-07）；身份/来源/必填冲突 → 拒绝（AC-10）；行业缺失保留并标记（AC-05）、行业冲突降级（AC-11）。

## 4. 明确 UNANCHORED（不给语义）

`base.dbf` 全部字段（SC/DY/HY/GXRQ/SSDATE 等；SC 取值 {0,1,2,9} 仅为机械分布）；`base.map` 尾部序号；`infoharbor_ex.code` f2/f3（关键词样值，如 `平安保险,谢永林,冀光恒`）；`tdxhy2.cfg` 的角色与超集语义；“tdxhy.cfg 行=当前上市”超出已锚定退市缺席观察的部分；东财二级分类（官方文件提供的是 TDX 研究行业，不是东财分类）。

## 5. AC 验收矩阵（详见 `result.json`）

| AC | 判定 | 依据 |
|---|---|---|
| AC-01 | VERIFIED_MECHANICAL | member sha/行数/字段位/编码探测全部复核 |
| AC-02 | PARTIAL | 身份/名称/来源映射成立；6 成员全包无名称（如实列出） |
| AC-03 | VERIFIED_MECHANICAL | 5230/5230 解析 L2/L1；`688688` T 侧 `T00` 残差 |
| AC-04 | VERIFIED_MECHANICAL | 缺名 6、空 T/X 异常码 3、dbf/map 重复码 `000020/200020`、跨文件 surplus 枚举 |
| AC-05 | VERIFIED_MECHANICAL | #167 固定样本行串逐字重放 |
| AC-06 | REPORTED | 分节报告；gen-1→gen-2 漂移见 #161；#71 使用边界不动 |
| AC-07 | REPORTED | 见下表 |

## 6. 给 #6 的结论（AC-07）

| 契约项 | 状态 |
|---|---|
| 沪深A 范围规则（f0∈{0,1}+T/X 非空）+ 排除枚举 | 可实施（本代 5230 成员） |
| 交易所枚举、身份（f0,f1）、保前导零 | 可实施（已锚定） |
| 名称 join（ex.code f0→f1） | 可实施，须带“缺名 6 成员”处置裁定 |
| 行业二级归属+名称+层级 | 可实施——但为 **TDX 研究行业二级**，非 #6 确认的东财二级；是否接受该分类标准替换是 #6 层面裁定，不在本切片 |
| 快照/消失/冲突/降级保护 | 可实施（沿用 #6 规则，逐条映射） |
| “当前上市”完整性断言、base.dbf/tdxhy2/ex.name 字段语义、成员修订规则 | 仍阻塞（UNANCHORED，需 #71 长期获取边界与更多证据） |
| 生产可用性 | 不适用——本切片不接生产代码，`NOT_PRODUCTION_READY` 不变 |
