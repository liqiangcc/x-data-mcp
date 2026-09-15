# issue-46-anchor-binding — 候选锚点形式化绑定验证

refs #46, #54。上游:evidence/issue-54-46-authority-discovery(#163,merged bf1fda7)。
本切片把"覆盖一致"升级为逐码绑定测试。**机械证据,不声称契约。**

## 绑定测试对象

三方 per-code 比对(union 620 个 T/X 码):

| 源 | 角色 | 层次 |
|---|---|---|
| `industry-classification.pdf`(官方红宝书文档,2026-06-30) | PDF 码表 code→name→level→index | 官方文档声明 |
| `gbbq.zip!zhb.zip!incon.dat` #TDXNHY/#TDXRSHY | 包内 code→name 字典 | 官方字节(绑定关系未声明) |
| `gbbq.zip!zhb.zip!tdxzs3.cfg` | name→index→code 表 | 官方字节(同上) |
| `tdxhy.cfg` f2(T)/f5(X)、`tdxhy2.cfg` f2(ZX)/f3(CJ)/f4(ZZ) | 字段 token 集合 | 待绑定对象 |

## 结果(binding.json)

- **unexplained = 0**:620 行 union 中每条差异都归入已解释类。
- 差异类分布:`MATCH` 1176、`IDX_MATCH` 596(PDF↔tdxzs3 索引码 0 冲突)、`NAME_WORDING_VARIANT` 40(全部截断/同义,逐条列出)、`NAME_DISPLAY_ALIAS` 13(tdxzs3 一级聚合 `TDX X` 显示别名)、`PDF_MISSING_CODE` 2(`T00` 文件内退化值;`X1203` PDF 缺二级行但其三级子码 X120301 存在)、`TDXZS3_MISSING` 7(PDF 有码无指数条目)。
- PDF 声明数 vs 表列数:T 三级 109 声明 vs 76 表列;X 二级 128 vs 127、三级 352 vs 316 —— incon.dat 分区计数与表列一致(145 T / 474 X,差异仅 X1203),两个独立官方 member 互相印证表列集,声明数差异记为已解释不对称。
- 字段覆盖:tdxhy.cfg f2 T 110/111 ∈ PDF/incon(缺 `T00`);f5 X 349/349 ∈ 两者;tdxhy2.cfg ZX 279/279、CJ 314/314 ∈ incon;**ZZ 仅 4/256**(同前缀不同码代)。

## 绑定强度分级(如实)

- **S1 码空间锚定**:PDF 官方文档声明 T/X 码空间的码、名、层级、指数码 —— 成立。
- **S2 字典一致**:incon.dat/tdxzs3.cfg 与 PDF 表在已解释差异内一致 —— 成立(机械)。
- **S3 文件字段绑定**:tdxhy.cfg 字段值 ⊂ 已锚定码空间 —— **必要非充分**;PDF/文档未声明 tdxhy.cfg 字段就是该码空间,字段位置归因仍是推断。

## UNANCHORED(无源覆盖)

- tdxhy.cfg f0 字段位置归因(Dict.html 仅供给 0/1/2 枚举词表)
- tdxhy.cfg f1 code↔证券名 的文件角色;f3/f4(gen-2 恒空)
- tdxhy2.cfg f1/f5 角色与空值语义
- tdxhy.cfg/ex.code/base.dbf 的 A 股范围边界(HSStockKind 枚举仅供给词表)
- tdxhy2.cfg f4 ZZ 码代(incon ZZHY 4/256)
- **GBBQ C1–C4 二进制绑定**

## #54 结论

已扫全部入库官方文档(tq-interface.pdf 全文 extract、calc-rules.doc、全部 TQ HTML 页):**未发现**任何把 GBBQ 二进制记录 C1–C4 绑定到业务字段顺序/单位/float32 表示/舍入的表述。官方文档停在"业务字段名"层(`get_divid_factors`: Type/Bonus/AlloPrice/ShareBonus/Allotment);缺失"业务字段→二进制偏移/编码"绑定层。补此层仅剩两路:官方客户端行为输出(本机 NOT_RUN)或公开官方格式规范(未发现)。

## 文件与复现

- `plan.json` 冻结计划;`scan_anchor_binding.py` scanner;`test_scan_anchor_binding.py` 8 测试。
- `industry-classification-extract-raw.txt`、`tq-interface-extract.txt` — pdftotext 派生文本(pin 在 plan)。
- `binding.json` 结果;`determinism.json` 双跑证据。

```sh
python3 scan_anchor_binding.py --output binding.json
python3 scan_anchor_binding.py --check binding.json
python3 -m unittest test_scan_anchor_binding -v
```
