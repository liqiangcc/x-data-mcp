# Issue #54——TQ oracle 判别 harness(freeze v1)

状态:**诊断 harness,非裁决工具**。输出永远是 `DIAGNOSTIC_ONLY`;
不退休 #54,不动三 blocker,不推断官方语义。

## 0. 目的

gen-3 Windows 采集(#217)将带回 TdxQuant `get_divid_factors` +
`get_market_data`(none/front)对 7 只冻结证券的逐字输出。本 harness
在数据到达**之前**完成三件事:

1. 机械提取冻结集在 gen-2 member 中的全部事件 + `.day` 前收盘上下文
   (`oracle-inputs.json`,现在就跑,真实证据);
2. 冻结 oracle 输入 JSON schema(TQ 逐字输出如何落盘);
3. 实现判别比较器:字段映射 × 十进制解释 × QFQ 链三个轴上,把
   oracle 值与 member float32 逐事件/逐字段比对——oracle 一到即出
   判别矩阵。

## 1. 冻结输入(pin 值,全部既有证据)

| 输入 | pin |
|---|---|
| gbbq member | `/home/box/retained-inputs/official-gbbq-member`,sha256 `4a3e9b86…9d77`,205776 条 ×29B |
| community decoder | `/home/box/retained-inputs/gbbq_reader.py`,raw sha256 `e28fae76…f6ba` |
| hsjday.zip | `/home/box/acceptance-run-issue3/snapshot/hsjday.zip`,sha256 `c82f852a…0892` |
| 记录布局 | byte0 market(0=sz,1=sh);1–7 code;8–11 date u32;12 category;13–28 四×float32 C1–C4 |

## 2. 冻结 oracle 集(#217 §2 机械生成,7 只)

| 证券 | 判别价值 |
|---|---|
| sh.600004 / sh.600006 / sz.000002 / sz.000004 | #53 回归四股;sz.000004 @1993-05-24 cat1 四字段全非零(0.8/10/3/4);sz.000002 @1991-06-01 C2&C4 非零(4.4/5.0);sh.600006 @2004-06-11 送股 10 |
| sz.000001 | 两条 cat1 全四字段非零(1993-05-24:3/16/8.5/1;1994-07-11:5/5/5/1) |
| sh.688318 | 官方文档示例:2020-09-29 Type=1 Bonus=6.0 ↔ member C1=6.0(已佐证 C1=Bonus) |
| sz.000035 | cat15 `重新调整` @2012-11-30 |

## 3. oracle 输入 schema(`issue54.tq-oracle.v1`,冻结)

```json
{
  "schema": "issue54.tq-oracle.v1",
  "generator": {"kind": "tq_api|client_manual|mechanical_fixture",
                "captured_utc": "…", "environment_note": "…"},
  "securities": {
    "sh.688318": {
      "divid_factors": [{"date": "2020-09-29",
        "fields": {"type": "1", "bonus": "6", "allot_price": "0",
                   "share_bonus": "0", "allotment": "0"},
        "verbatim": "optional raw TQ row"}],
      "front_adjusted": [{"date": "2020-09-28", "close": "…"}],
      "unadjusted":   [{"date": "2020-09-28", "close": "…"}]
    }
  }
}
```

所有 oracle 值为**十进制字符串逐字**(不写 float);缺失字段显式
`null`;`generator.kind` 决定证据等级(tq_api > client_manual >
mechanical_fixture,后者仅自检用)。

## 4. 判别轴(比较器产出,全部 DIAGNOSTIC_ONLY)

- **轴 A 字段映射**:对每个 (oracle_field × C_field) 统计
  oracle↔member 同现事件的值一致性,输出每字段最优映射与歧义度;
  oracle=0 要求 member 对应位 ==0(缺失映射显式列出)。
- **轴 B 十进制解释**:五候选(frozen_repr / exact_binary /
  shortest_roundtrip / fixed_4dp / fixed_6dp)在轴 A 命中的字段对
  上,各自精确匹配率。
- **轴 C QFQ 链**:对每十进制解释 + 冻结参考式
  `(close*10 − c1 + c4*c2) / (10 + c4 + c3)`,沿 .day 链预测
  front-adjusted 收盘,与 oracle `front_adjusted` 逐日精确/容差
  比对。

输出:`matrix.json`——每 (事件,字段,候选) match/mismatch/
absent + 每候选存活计数。**不合并成单一 winner**;多候选同分
如实并列。

## 5. 自检(非 oracle)

`selftest` 用 mechanical_fixture oracle(由某候选解释机械生成的
伪 oracle)验证比较器:喂 candidate-k 派生 oracle 应只存活
candidate-k。证明的是比较器力学,不是语义。

## 6. 边界

- 不引入新解码假设;member 解码全部走 pin 的 community decoder;
- 无 oracle 时 `compare` 拒绝运行(schema 校验 fail-closed);
- 结果永不写 `VERIFIED`;#54 退休仍需 oracle + FG 裁定。
