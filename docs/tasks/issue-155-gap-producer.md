# #155 proven-gap producer：机器可复核 gap verification decision 契约（第一阶段设计冻结）

状态：设计冻结提案，供协调决策/Final Gate 评审。**本文件不落代码、不改
wire/schema、不创建 producer；producer 保持 fail-closed，系统继续
`NOT_PRODUCTION_READY`，#54/#46/#71 门禁不因本设计解除。**

基线：`origin/main` = `5e69c4b635c351bf7f9c9875d4cb0314b1c8e434`；
分支 `issue-155-gap-producer-design`，worktree
`/home/box/x-data-mcp-gap-producer-design`。

已核对的精确提交（本文件全部断言以上述树为准）：

| 内容 | 提交 |
|------|------|
| 当前基线（含 Slice 4 执行+covering，PR #154） | `5e69c4b635c351bf7f9c9875d4cb0314b1c8e434` |
| Slice 3 default wire 受理（PR #153） | `1b010f3` |
| Slice 2 internal default resolver（PR #152） | `fd8bbda` |
| availability 只读接入（PR #150） | `8a16b6f` |
| schema v11 事实层（PR #149） | `7089f4d` |

## 冻结的 producer 边界（#147 Final Gate §4，逐字适用）

- 首期只允许"显式、机器可复核的 gap verification decision"产生
  `proven_gaps`；必须原子绑定 `symbol + period + start_date +
  end_date + evidence_id + evidence_sha256`，结论
  `confirmed_missing_range`（或等价枚举）。
- 明确禁止触发源：普通 task failed/cancelled/interrupted；source 返回
  0 行；日期不连续/缺行；`coverage_status/reason` 自由文本；版本缺
  席/当前日期/交易日历推断；仅"请求过该范围"或"未完整取得"。
- `gap_coverings` 已由 #154 落地：仅 frozen 列表内 gap 且整段 ⊆ 已验
  证提交 union range 才追加；overlap 不构成覆盖；历史行不删。
- 未来 `verified_task_outcome`/`publication_coverage_decl` producer 须
  达到同等机器可复核标准；generic 记录不自动升级。

---

## 1. 现有 durable facts 逐项盘点：能否充当 `confirmed_missing_range` 决定

判定标准（来自冻结边界）：决定必须 (i) 机器可复核——存证对象可按
digest 重放/重算；(ii) 绑定明确区间 `symbol+period+[start,end]`；
(iii) 表达**确认缺失**这一结论本身，而非"未取得/未提交"。

### 1.1 task 终态与 item outcome（`tasks`/`task_items`，001/007）

- 能证明：item 曾启动、曾以 success/failure 收尾、任务被取消/中断；
  `ItemOutcome.error` 是持久化错误字符串（如
  `"SOURCE_NOT_VERIFIED: ..."`、`"task changed or cancelled..."`），
  由 `runner.rs` 错误路径写入。
- 不能证明：**源端某区间无数据**。failure outcome 只断言"本次执行未
  完成提交"，其 error 是未分类自由文本；success outcome 断言"已提交
  某版本"而非"某区间无数据"。"未取得"≠"确认缺失"——恰是被禁触发
  源。无字段承载 `confirmed_missing_range` 结论或区间绑定。
- **裁定：NOT_OK。** generic task outcome 是 Final Gate 明令禁止的触
  发源，且结构上不携带区间级缺失结论。

### 1.2 核实巡检回执 `verification_inspections`（009；
`verification_inspection/decision.rs`）

- 能证明：对某 verification task 的巡检结论
  （`AlreadyFinished`/`Interrupted`/`InsufficientEvidence`/
  `NotApplicable`），audit_json+audit_sha256 持久；`validate()` 可
  以将 decision 从 facts+observation **确定性重放比对**——这是当前
  代码库中"机器可复核决定"的最强先例。
- 不能证明：数据缺口。巡检对象是任务生命周期事实
  （registration/owner/result/finish 状态），conclusion 词汇表无
  range、无缺失语义；`Interrupted` 仅断言"已核实停止"，不涉行情内
  容。
- **裁定：NOT_OK**（作为 gap 证据）。但其"密封 decision → 原子
  audit 提交 → digest 重放校验"的三段式形态正是 §3 producer 契约应
  复用的结构模式。

### 1.3 停止写核实回执 `stopped_write_checks`（007；
`task_verification/decision.rs`）

- 能证明：目标任务"写者已停、持久效果已确定"的核实结论，含
  `target_before`/`after` 快照与 `snapshot_sha256` 绑定；
  `validate_audit` 同样支持确定性重放。
- 不能证明：区间缺失。`Observation::Stopped` 的证据是 writer-stop
  证据（evidence_ref），非数据缺失证据；`InsufficientEvidence` 显式
  不构成任何终态断言。
- **裁定：NOT_OK。**

### 1.4 发布覆盖声明（`data_versions.coverage_status/coverage_reason`；
`MarketDataBatch.coverage`/`version_evidence`）

- 能证明：版本自报的 `{status, reason}`（enum
  Complete/Incomplete/Unknown + 自由文本）与字段级证据状态——写入
  时由候选记录带入的**自报元数据**。
- 不能证明：逐证券日期区间缺口；`Incomplete` 不携带区间，reason 为
  自由文本，且整个 coverage 声明不绑定"已核实检查源端"的事件——自
  报标签不是 verification decision。
- **裁定：NOT_OK。** Final Gate 已明确把 `coverage_status/reason`
  自由文本列入禁止触发源；升级为 producer 需先达到 §1 判定标准，
  现状差距是本质性的（无区间、无 digest 绑定的检查事件、无结论枚
  举）。

### 1.5 `market_update_results`/`yearly_execution_results` +
`range_execution_results`（005/006）

- 能证明：某 task/item 以 `base_version_id` 产出 `result_version_id`
  （或 no_change 复用）；audit_json 内嵌 `range`/`segment`/`evidence`
  （base+candidate digest、`same_qfq_basis_ref`）/`combined` 覆盖证
  明，读路径 `read_result` 对全部字段+digest 做完整复核——**这是
  "已验证存在/覆盖"方向的机器可复核事实**，即 #154 covering 写回的
  输入侧。
- 不能证明：缺失。即使 no_change 也只断言"无新提交"；segment 为空的
  结果（若存在）仍属"未取得"而非"确认缺失"，且"source 返回 0 行"
  已被明令禁止。
- **裁定：NOT_OK**（作为 gap producer）。它是 covering/存在性证据，
  方向相反；不得反向解读为 gap 证据。

### 1.6 `task_item_execution_starts`（010）

- 能证明：collection item 曾真实启动（INSERT-only 单调标记）。
- 不能证明：任何结果或缺失语义——设计目标即"只证明启动"。
- **裁定：NOT_OK。**

### 1.7 汇总

| 候选事实 | 机器可复核 | 区间绑定 | 确认缺失结论 | 裁定 |
|----------|-----------|----------|--------------|------|
| task/item 终态 outcome | 部分（receipt/audit 可核，error 为自由文本） | 无（item scope 是 symbol×period） | 无 | **NOT_OK**（禁止源） |
| verification_inspections | 是（digest 重放） | 无（task 级） | 无 | **NOT_OK** |
| stopped_write_checks | 是（digest 重放） | 无（task 级） | 无 | **NOT_OK** |
| coverage_status/reason | 否（自报自由文本） | 无 | 无 | **NOT_OK**（禁止源） |
| update/yearly/range results | 是（audit 全字段+digest 复核） | 有（audit.range） | 无（方向为存在/覆盖） | **NOT_OK** |
| execution_starts | 是 | 无（启动事实） | 无 | **NOT_OK** |

**结论：`GAP_DECISION_SOURCE_NOT_AVAILABLE`。** 现有全部 durable
facts 均不构成"确认缺失区间"决定——缺口是本质性的（无结论枚举、无
区间绑定的缺失语义），不是实现差距。需要最小 producer 契约（§3）。

---

## 2. 禁止路径重申（设计约束，非实现细节）

以下任何形式均不得被解释、扩展或降级为 producer 输入：

- 任何 task failed/cancelled/interrupted/`OutcomeUnknown`/
  `NeedsVerification` 终态或其错误字符串；
- provider/source 返回 0 行或空 segment；
- 相邻版本日期不连续、`first_date` 突变、版本缺席；
- `coverage_status`/`coverage_reason`/`unresolved_ranges` 文本；
- 当前日期、Asia/Shanghai 时钟、交易日历推断；
- "请求过该范围"/"受理过该 scope"本身；
- `Evidence::Missing`（publication_verification 已明示 NOT
  'not published'）；
- covering 缺失（open 状态是 consumer 读取投影，不是缺失证据）。

---

## 3. 最小 producer 契约提案（设计提案，未实现）

复用既有 verification 家族的三段式：密封 decision port（trusted
implementation，非 Deserialize 声明）→ 原子 audit/decision 提交 →
digest 可重放校验。

### 3.1 decision 对象（durable evidence）

新 durable 事实对象 `GapVerificationDecision`（逻辑形态；存储形态见
3.3 选项，需 schema 评审——OPEN-QUESTION-3）：

```
{
  decision_id:        String,        // 证据对象身份（= proven_gaps.evidence_id）
  symbol:             String,
  period:             'day'|'year',
  start_date:         Date,
  end_date:           Date,          // start<=end
  conclusion:         'confirmed_missing_range',   // 首期唯一可产生 gap 的结论
  basis_ref:          String,        // 底层检查依据对象 id（见 §4）
  basis_sha256:       String(64hex), // 依据对象 digest
  decided_by:         String,        // 签发者身份（见 OPEN-QUESTION-2）
  decided_at:         DateTime,
}
decision_sha256 = sha256(canonical(decision))  // = proven_gaps.evidence_digest
```

非 `confirmed_missing_range` 的结论（如 `data_present`/
`inconclusive`/`check_failed`）是合法 decision 结果但**不产生**
`proven_gaps` 行——decision 与 gap 事实必须区分。

### 3.2 producer 接线（原子 + 幂等 + fail-closed）

- **写入时机**：仅在显式 decision 输入（命令/任务）执行且 decision
  对象通过完整校验后写入；校验 = 重算 `decision_sha256` 与对象内容
  一致 + 结论枚举 + 区间合法 + basis_ref 对象存在且 digest 匹配
  （basis 校验边界见 OPEN-QUESTION-1）。
- **原子性**：decision 对象持久化与 `proven_gaps` 行在**同一写事务**
  内提交（沿 `StartItem`+`task_item_execution_starts`、
  `commit_range` 先例）；decision 存在但 gap 行缺失（或反之）是
  integrity 违例，读路径 fail-closed。
- **幂等**：同一 `decision_id` 重放 identical decision → no-op；
  同 `gap_id` 不同内容 → identity conflict（沿用
  `record_proven_gap` 既有语义）；同 `decision_id` 不同内容 →
  integrity 冲突 fail-closed。
- **存储层边界不变**：`record_proven_gap::validate()` 保持纯机械校验
  （其文档明示 evidence-object 存在性/digest 属 application 职责）；
  语义校验全部在 producer 层。

### 3.3 存储形态选项（需评审——OPEN-QUESTION-3）

- **A. 新表 `gap_verification_decisions`**（decision_id PK + 全字段 +
  `decision_sha256` + INSERT-only 触发器）：decision 是一等 durable
  fact，audit 可独立查询；代价 = schema v12。
- **B. decision 对象存既有 evidence/audit 通道**（如随 verification
  task 的 result/audit_json）：不加表，但 decision 不再是独立可查询
  对象，digest 重放依赖 audit 结构。
- 推荐 A（与 `proven_gaps` 对称、独立可审计），最终以 schema 评审为准。

### 3.4 输入契约形态（需评审——OPEN-QUESTION-4）

- **A. verification 家族新契约**：`verify_data_gap` 命令，dataset 复用
  `task.verification` 或新增 `market.gap_verification`；单 item 绑定
  `{symbol, period, start_date, end_date, decision}`，decision 对象经
  frozen_inputs 冻结后由 runner 校验+写库。优点：复用
  Claim/occupancy/audit/inspection 全部机制。
- **B. 直接 application API**（`SqliteMarket::record_gap_decision`）：
  无任务编排，适合 operator 导入场景；缺点：无受理/并发/审计链路。
- 推荐 A（与既有 verification 决策同构，天然获得 atomic commit +
  审计 + NeedsVerification 语义）。

---

## 4. 证据本体要求与显式 OPEN-QUESTION

**decision 必须是什么**（可冻结部分）：

1. durable 对象：有身份（decision_id）与内容 digest（canonical
   sha256），任何读取方可重算验证；
2. 绑定明确 `symbol+period+[start,end]`，字段强类型非自由文本；
3. 携带结论枚举；仅 `confirmed_missing_range` 可产生 gap；
4. 携带 `basis_ref+basis_sha256` 指向"检查了什么"的底层依据对象；
5. 决定可重放：`validate`-式确定性重算（沿 `validate_audit` 先例），
   存证与重算不一致 → integrity fail-closed。

**OPEN-QUESTION-1（Final Gate/source 授权）**：`basis` 对象的合法性
边界——什么构成"已核实源端无该区间数据"的底层依据？候选：(a) 安装
的 trusted source-verifier port 输出（类 `StopInspector` trait，不
可 Deserialize）；(b) operator 签发的 decision 文件+独立可核依据；
(c) 真实 source 查询协议（依赖 #54/#46/#71，当前全部 fail-closed）。
**本设计不预设 basis 语义**；在 basis 可机器复核之前，producer 可实
现但真实来源路径继续 NOT_RUN。

**OPEN-QUESTION-2（产品/Final Gate）**：`decided_by` 签发者授权模型
——谁/什么组件有权签发 `confirmed_missing_range`？（本地 operator、
安装的 verifier 插件、还是 source 自身声明？）未经授权模型的 producer
保持 fail-closed 默认值：decision 校验必须包含签发者白名单或等价授
权检查，具体机制待裁定。

**OPEN-QUESTION-3（schema 评审）**：decision 存储形态（§3.3 A/B）。

**OPEN-QUESTION-4（契约评审）**：输入形态（§3.4 A/B）、
`contract_version`（建议独立 `gap-verification-task-v1`，不复用既有
key）、dataset 命名、`year` period 是否首期纳入（
`proven_gaps.period` 已支持 'year'，但 yearly gap 的 basis 语义可能不
同——建议首期仅 'day'，待裁定）。

**OPEN-QUESTION-5（产品）**：decision 与 default-range consumer 的
时序——`proven_gap_ranges` 在受理时冻结，decision 落地晚于受理的
gap 自然进入下一次受理；是否需要"decision 触发自动 backfill 任务"属
产品编排决策，本设计不要求。

---

## 5. 测试矩阵（供后续切片复用）

| 层 | 测试 |
|----|------|
| decision 校验 | `decision_sha256` 与内容不符 → fail-closed；缺字段/未知字段/非法区间(start>end)/非64hex digest → 拒绝；非 `confirmed_missing_range` 结论不写 `proven_gaps` |
| 原子性 | decision 写入成功但 gap 插入失败（注入）→ 整事务回滚，无孤儿 decision；反之亦然 |
| 幂等 | 同 decision_id 同内容重放 → no-op；同 gap_id 同内容 → no-op；同 decision_id 异内容 → integrity conflict；同 gap_id 异内容 → conflict（既有语义） |
| 禁止源回归 | task failed/cancelled/interrupted/0 行 segment/coverage_reason/'请求过范围'/`Evidence::Missing`/日期不连续 —— 全路径断言不产生 `proven_gaps` 行 |
| basis 绑定 | basis_ref 不存在/digest 不匹配 → fail-closed；basis 对象损坏 → 读路径 fail-closed |
| 端到端 | decision→gap→`get_data_availability` confirmed_gaps→default backfill 受理冻结该 gap→执行→`cover_gap`→open set 清空 |
| 重放/审计 | decision `validate` 重算不一致 → integrity；`proven_gap`/`gap_coverings` 读路径既有测试不回归 |
| 权限 | 未授权签发者 decision → 拒绝（机制依 OPEN-QUESTION-2 裁定后补） |

## 6. 建议最小实现切片（互不合并，各自独立评审）

1. **Slice P1 — decision 事实存储**：`gap_verification_decisions` 表
   （若选 §3.3-A）+ 连接级读写原语 + 机械校验 + 触发器测试；不接
   producer、不接 wire。**阻塞项：OPEN-QUESTION-3。**
2. **Slice P2 — producer 契约**：`gap-verification` 契约 + decision
   校验 + 同事务 decision+gap 写入 + 幂等/冲突语义；fixture
   verifier port（类 `SyntheticRangeProvider` 先例），真实 basis 校验
   保持 fail-closed。**阻塞项：OPEN-QUESTION-1/2/4。**
3. **Slice P3 — wire/入口**：若选 §3.4-A 任务契约则加 public 命令映
   射；否则 application API 评审后定。
4. consumer/reader/covering 已由 #149/#150/#152-#154 落地，无需切
   片。

每个 slice 前需对应 OPEN-QUESTION 获 Final Gate 裁定；未裁定项一律
保持 fail-closed。

## 7. 本设计明确不做

- 不实现任何 producer 代码、schema、wire；
- 不解除 #54/#46/#71，不宣称 NOT_PRODUCTION_READY 改变；
- 不定义 basis 证据的来源协议（属 source-gate 领地）；
- 不提议任何 generic 失败/0 行/日期不连续/自由文本触发路径；
- 不要求 `proven_gaps`/`gap_coverings`/`record_proven_gap` 语义变更
  （现有机械校验边界保留）。
