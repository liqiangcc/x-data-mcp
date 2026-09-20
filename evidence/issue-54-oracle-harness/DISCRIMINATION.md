# Issue #54——TQ oracle 判别分析（gen-3 matrix 实跑后）

状态：**DIAGNOSTIC_ONLY**；`GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED` 保留。
输入：`evidence/issue-217-gen3-acquisition/matrix.json`（7 只冻结证券 × TQ type=4/type=1 逐字 oracle）。
产出：`discrimination.json`（`python3 discriminate.py <matrix.json> discrimination.json` 可机械复算）。

## 判别结论

### 1. 字段映射——oracle 已证实

非零 oracle 值上的跨字段支持矩阵（`axis_a_mapping_support`）无歧义：

| oracle 字段 | member 字段 | 直接精确命中 | 解析后合计 |
|---|---|---|---|
| `bonus` | **C1** | 101/118 | **118/118** |
| `allot_price` | **C2** | 8/8 | 8/8 |
| `share_bonus` | **C3** | 40/41 | **41/41** |
| `allotment` | **C4** | 7/8 | **8/8** |

非对角项命中 ≤3/118（噪声水平）。社区文档假定的 `bonus→C1 / allot_price→C2 / share_bonus→C3 / allotment→C4` 映射现由官方 oracle 独立证实。

### 2. member float32 值是业务十进制——oracle 已证实（在 2dp 显示精度内）

映射字段对上全部 175 个非零比较：156 个在 shortest-roundtrip 解释下即精确相等；
其余 19 个 **全部** 满足 `quantize(member, 0.01) == oracle`
（`mapped_pair_residual_after_2dp = []`）。例：

- member `1.7999999523162842` ↔ oracle `1.80`——float32 尾差是表示噪声；
- member `0.423`/`10.45102`/`9.761257` ↔ oracle `0.42`/`10.45`/`9.76`——member
  携带比 oracle 2dp 显示更高的有效小数位，二者在 2dp 上一致。

**含义**：GBBQ C1–C4 存储的是业务十进制值，float32 编码引入的 ±1ulp
长尾不是业务值的一部分。这正面回答了 #54 问题 1 的一半（"是十进制业务值"），
但没有回答"规范 scale 是多少"——见下。

### 3. 十进制解释候选——本 oracle 无法判别

| 候选 | 映射对精确命中 | 链 exact | 链 within 0.005 |
|---|---|---|---|
| `shortest_roundtrip` | 156/175 | 1661 | 35613/37329 (95.40%) |
| `fixed_4dp` | 156/175 | 1661 | 35611 (95.40%) |
| `fixed_6dp` | 156/175 | 1661 | 35613 (95.40%) |
| `exact_binary` / `frozen_repr` | 99/175 | 1216 | 35814 (95.94%) |

- 三个规范化候选得分相同是因为：它们产生分歧的输入恰是 member 携带
  3–6dp 子分精度、oracle 只显示 2dp 的 19 个事件——这些事件在所有
  候选下都不是精确命中，oracle 精度不足以把任何候选推过对方。
- 规范化候选在字段层显著优于原始 float32 解释（156 vs 99），并在链层
  取得更多 exact（1661 vs 1216）——但 exact 优势部分来自 oracle 自身
  2dp 舍入与"干净"预测值重合，判别力有限。
- **没有任何候选复现 oracle 前复权链**：within-0.005 均停在 ~95-96%，
  且逐股分裂——规范化候选在 `sh.600006` 上多覆盖 +244，却在
  `sz.000001`/`sz.000002`/`sh.688318` 上各少覆盖 42–245。链层 miss
  是逐事件的系统性偏移段（同一 predicted 值跨多日重复），不是噪声。

### 4. 链层残余分歧的性质

轴 C miss 显示 oracle 前复权序列与"冻结社区参考式
`(close*10 − c1 + c4*c2)/(10+c4+c3)` 沿 .day 复合"在特定事件窗口
持续偏离（如 `sh.600006` 1999-08-12 起 oracle 0.95 vs predicted 0.94497）。
可能成因（未裁定）：oracle 的调整因子数据源与 gbbq 记录在这些窗口
不一致，或 TQ 前复权算法与社区参考式不同。两种成因都不能用本矩阵
内的数据区分。

## 对 #54 验收边界的回答

- 规范化规则是否"有可审计依据"：字段映射与"值是十进制"已有 oracle
  证据；**规范 scale/舍入步骤仍无官方依据**——三个候选在可得精度内
  不可区分，选任一都超出证据。
- `600006` 半分边界：oracle **已证实** `bonus=0.05`（member C1
  `0.05000000074505806` 在三个规范化候选下全部 `match=True`）——
  #53 的 counterfactual"业务值恰为 0.05"获得官方 oracle 独立佐证；
  但 `(7.25*10−0.05)/10 = 7.245` 的舍入位置仍无证据（oracle 不给
  参考价计算管线的中间舍入）。
- 结论：维持 `GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED`
  / `TDX_OFFICIAL_QFQ_CONTRACT_NOT_PROVEN`。要把本 Issue 推到 PASS，
  需要一个能暴露 >2dp 规范值或绑定计算管线的官方证据源（如官方客户端
  逐字显示、官方文档的字段精度定义），TQ type=4/1 的 2dp oracle
  已用尽其判别力。
