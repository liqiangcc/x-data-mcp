# issue-383：`qfq-exact-v1` 的 Python `Fraction` 参考实现与夹具

M1 PR-3（设计：[`docs/tasks/issue-383-m1-research-data.md`](../../docs/tasks/issue-383-m1-research-data.md) §4.3）
把前复权从"28 位 `Decimal` 链 + 2dp 舍入"换成"精确有理数链 + 末次一次舍入"。本目录是这条
断言的**语言无关 oracle**：

- `reference.py`：只用标准库（`fractions.Fraction`）实现复权链与末次舍入；
- `expected.json`：夹具（输入 + 期望值），Rust 侧 `tests/algorithms/qfq_exact.rs` 逐位比对；
- `test_reference.py`：CI 用 `unittest` 重算一遍，保证夹具本身可复现。

## 为什么需要它

2dp 前复权在原理上不可跨实现复现：实测 8.44% 的价格字段恰好落在 2dp 半分位，此时舍入方向
由中间精度决定（28 位近似 vs 精确）。`qfq-exact-v1` 的判据因此只能是"值只取决于（bar、事件、
精度、算法版本）"，而这条判据必须有一个与被测实现**不同**的实现来验证——就是这里。

## 输入（全部是已提交的证据，不读生产库）

| 输入 | 位置 |
|---|---|
| `.day` 成员（3 只证券） | `evidence/issue-166-windows-official-behavior/raw/files/{sh600004,sh600006,sz000002}.day` |
| cat1 事件（94 条冻结域） | `evidence/issue-54-gbbq-full-events/result.json` |
| 合成边界用例 | `expected.json` 的 `cases`（输入与期望值放在同一处，Rust 侧喂同一份数字） |

## 断言的三件事

1. **语料摘要**：3 只证券全部 bar × 4 个价格字段（82,316 个字段）按固定行格式做 sha256，
   与 `corpus.digest` 相等。行格式见 `expected.json` 的 `digest_format`。
2. **边界用例**：空事件集（恒等链）、未来事件（对所有 bar 生效）、事件日当天的 bar 不复权
   （严格 `>`）、同日多事件按输入顺序合成、2dp 半分位（0.025 → 0.03）等。
3. **半分位计数**：精确值恰好落在 2dp 半分位的字段数（本语料 2,992 个，6dp 半分位 0 个）。
   AC-M1-3 的"差异逐条可枚举"就是靠这个分类：Rust 侧断言"6dp 按 2dp 舍入后与旧 2dp 不同"
   的字段必须全部落在这个集合里。

## 用法

```bash
python3 -B evidence/issue-383-qfq-exact/reference.py --check    # 校验（CI 跑这条）
python3 -B evidence/issue-383-qfq-exact/reference.py --write    # 重新生成期望值
```

## 已知边界

- 这只证明"同一份输入 + 同一算法 → 同一输出"，不证明算法本身符合官方语义；语义依据仍是
  #54/#167/#169 的冻结契约（`docs/source-closure.md` 门禁第 1 条）。
- `reference.py` 的 28 位 `Decimal` 模拟只用于离线统计（复现设计 §16.1 的差异率），**不**作为
  旧 2dp 基线的逐位 oracle：那是 Rust 侧 `qfq::forward_adjust_reference` 的职责。
- 语料只覆盖 3 只证券（`.day` 成员已提交的那 3 只），不外推成全市场比例。
