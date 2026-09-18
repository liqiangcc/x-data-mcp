# Issue #54——TQ oracle 判别 harness

**诊断工具,非裁决。** 所有输出 `DIAGNOSTIC_ONLY`;不退休 #54。

## 现状

- `oracle_compare.py extract oracle-inputs.json` — **已执行**:从 pin 的
  gen-2 member(205776×29B,sha `4a3e9b86…9d77`)+ hsjday.zip 提取
  7 只冻结证券的 **365 条事件** + 每条 cat1 事件的前收盘上下文 +
  每证券全 .day 收盘序列。
- `selftest` — **PASS**:mechanical_fixture oracle 下,生成映射对
  336/336 命中、链 40/40 精确、生成解释在最优并列中
  (shortest_roundtrip 与 fixed_6dp 并列——≤6dp 值判别力相同,
  如实并列不裁断)。
- `compare <oracle.json>` — 等待 gen-3 TQ 采集(#217);schema
  `issue54.tq-oracle.v1` 冻结于 PLAN.md §3。

## 开发中发现的真实事实

同 (symbol,date) 在 member 中可有**多 category 记录**(sz.000004
@1992-04-27 等):初版按 date 匹配会把 cat1 oracle 错配到其他
category 记录——已修正为按 (date, oracle `type`→category) 匹配,
并把 `same_key_records` 歧义计数如实记录。这正是 harness 先于真实
oracle 预建的价值:bug 在自检阶段暴露,不在裁决阶段。

## oracle 到手后

```bash
python3 oracle_compare.py compare oracle-inputs.json tq-oracle.json matrix.json
```

输出三轴判别矩阵:字段映射(of×cf×interp 命中率)、十进制解释
(每候选精确匹配率)、QFQ 链(每候选 predicted vs oracle
front_adjusted 精确/0.005 容差)。多候选同分如实并列;FG 裁定前
不存在 winner。
