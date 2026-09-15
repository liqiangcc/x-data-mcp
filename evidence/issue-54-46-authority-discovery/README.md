# issue-54-46-authority-discovery — 权威来源可行性盘点

refs #54, #46。Final Gate(#54 comment-5677537392 / #46 comment-5677541616)已封死拟合与结构推断路线;本切片只做**来源发现与分级**,不做任何语义断言。

## 证据分层(不混淆)

1. **official 字节**: retained-inputs 内 hash 钉死的官方包/官方文档。
2. **official 文档文本**: 官方网页/PDF 中可引用的字段名与表述。
3. **in-package 字典**: 官方包内 member(如 incon.dat)自带的 code→name 对 —— 官方字节但**未被官方文档声明绑定到 tdxhy.cfg 字段**。
4. **community RE**: gbbq_reader.py 等 —— 本切片不依赖其语义。
5. **diagnostic**: 覆盖率/一致性数字均为机械统计,非语义证明。

## 状态约定

- `VERIFIED`: 字节已获取、hash 钉死、来源 URL 记录。
- `CLUE_ONLY`: 官方字节/文档存在,但对目标语义无直接绑定。
- `PARTIAL_ANCHOR` / `DIRECT_ANCHOR`(anchor 字段): 对目标语义的潜在锚定强度,均为**候选**,未声称契约成立。
- `NOT_RUN`: 官方行为输出(Windows 客户端 / TQ 终端内 API)在本 Linux 环境不可执行。

## 关键初步观察(详见 sources.json)

- `official-gbbq.zip!zhb.zip!incon.dat` 是官方包内多命名空间字典,机械覆盖 tdxhy.cfg f5(X) 349/349、f2(T) 110/111、tdxhy2.cfg ZX 279/279、CJ 314/314;**ZZ 仅 4/256**(命名空间相同但码代不同)。
- 官方公开文档 `通达信板块指数和行业分类.pdf`(2026-06-30)含 T 代码表(13/56/109 级)与 X/研究行业表(30/128/352 级)+ 880/881 指数码;PDF 文本 token 覆盖 tdxhy.cfg X 349/349、T 110/111。
- TQ 官方文档 `get_stock_info` 样例 `rs_hycode_sim=X4202`/`rs_hyname=软件服务`/`blockzscode=881355`,与 incon.dat `X4202|软件服务`、tdxzs3.cfg `软件服务|881355|…|X4202` 三方一致(机械一致,非声明绑定)。
- `get_divid_factors` 官方字段表(Type 1=除权除息、Bonus、AlloPrice、ShareBonus、Allotment,沪深京范围)是 GBBQ 业务语义的**最接近候选**,但不绑定二进制记录 C1–C4 的顺序/单位/float32 表示。
- `get_market_data` 官方文档记录 `dividend_type` none/front/back 与 ForwardFactor 沿用规则;**未给出**复权计算公式或舍入策略。
- 官方 Windows 客户端 / TQ API: `NOT_RUN`(Linux 不可执行;不装 Wine;TQ 需在通达信终端内运行)。

## 文件

- `plan.json` — 冻结计划:输入 pins、URL 台账、规则、非目标。
- `scan_authority.py` — 独立 stdlib scanner;失败关闭校验输入 hash。
- `test_scan_authority.py` — 合成 fixture 测试。
- `industry-classification-extract.txt` — `pdftotext -enc UTF-8` 派生文本(committed,plan 内 pin hash;PDF 表格列在文本抽取中交错,只用 regex token)。
- `sources.json` — 生成结果(URL+sha256+获取时间+锚定范围+缺口)。
- `determinism.json` — 双跑 byte-identical 证据。

## 复现

```sh
cd evidence/issue-54-46-authority-discovery
python3 scan_authority.py --output sources.json
python3 scan_authority.py --check sources.json   # 期望 byte-identical
python3 -m unittest test_scan_authority -v
```

门禁状态不变:`GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED`、`TDX_OFFICIAL_QFQ_CONTRACT_NOT_PROVEN`、`FIELD_SEMANTICS_NOT_VERIFIED`。
