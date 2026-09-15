# #155 proven-gap producer：机器可复核 gap verification decision 契约（设计冻结）

状态：设计冻结，供协调决策/Final Gate 评审。**本文件不落代码、不改
wire/schema、不创建 producer；producer 保持 fail-closed，系统继续
`NOT_PRODUCTION_READY`，#54/#46/#71 门禁不因本设计解除。**

修订依据：本文档第二版按 **Final Gate 裁定（comment 5675608420，
2026-09-15，基于 PR #156 head `8eb485a`）** 修正；第一版 inventory 与
NOT_OK 逐项裁定获接受并保持原样，OPEN-QUESTION 表全部按裁定落为
RESOLVED。

基线：`origin/main` = `f4d42cefe938b8fcd075c250bfa1964d13549841`；
分支 `issue-155-doc-revision`，worktree
`/home/box/x-data-mcp-gap-producer-rev`。

已核对的精确提交（本文件全部断言以上述树为准）：

| 内容 | 提交 |
|------|------|
| 当前基线（含本文档第一版，PR #156） | `f4d42cefe938b8fcd075c250bfa1964d13549841` |
| 第一版设计文档 head | `8eb485a02ba2915dd9e61e6eb7785053d607db2c` |
| Slice 4 执行+covering（PR #154） | `5e69c4b635c351bf7f9c9875d4cb0314b1c8e434` |
| Slice 3 default wire 受理（PR #153） | `1b010f3` |
| Slice 2 internal default resolver（PR #152） | `fd8bbda` |
| availability 只读接入（PR #150） | `8a16b6f` |
| schema v11 事实层（PR #149） | `7089f4d` |

## 冻结的 producer 边界（#147 Final Gate §4 + #155 裁定 5675608420）

- 首期只允许"显式、机器可复核的 gap verification decision"产生
  `proven_gaps`；必须原子绑定 `symbol + period + start_date +
  end_date + evidence_id + evidence_sha256`，结论
  `confirmed_missing_range`。
- **`confirmed_missing_range` 语义（裁定 §1 冻结）**：相对于一个已验
  证的 published QFQ daily version，存在一个机器可复核的**本地数据
  覆盖缺口**，且该范围是后续显式 update/backfill 可重新获取/验证的
  候选范围。它**不是**"源端无该区间数据"：若 basis 证明源端明确不提
  供/不存在数据，该结论属 source limitation / not-available
  evidence，**不得**写入 `proven_gaps`——否则会把不可补的数据错误
  送入 backfill。
- **首期 `period=day` 唯一支持**。yearly 继续由 fixed published
  daily 本地派生，不单独产生 yearly proven-gap。
- 明确禁止触发源：普通 task failed/cancelled/interrupted；source 返回
  0 行；日期不连续/缺行；`coverage_status/reason` 自由文本；版本缺
  席/当前日期/交易日历推断；仅"请求过该范围"或"未完整取得"。
- `gap_coverings`（#154 已落地）：仅 frozen 列表内 gap 且整段 ⊆ 已验
  证提交 union range 才追加；overlap 不构成覆盖；历史 gap/decision
  行永不删除或改写。
- generic task/verification 记录不自动升级为 decision。

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

**结论：`GAP_DECISION_SOURCE_NOT_AVAILABLE`（Final Gate 已接受）。**
现有全部 durable facts 均不构成"确认缺失区间"决定——缺口是本质性
的（无结论枚举、无区间绑定的缺失语义），不是实现差距。最小
producer 契约按 §3 冻结。

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
- covering 缺失（open 状态是 consumer 读取投影，不是缺失证据）；
- **（裁定 §1 新增）source limitation / not-available 证据**：basis
  证明"源端明确不提供/不存在该区间数据"的结论不得写入
  `proven_gaps`——该范围不可补，进入 backfill 是错误信号；
- **（裁定 §2/§4 新增）caller 提交的结论/basis_sha256/
  decision_sha256/decided_by/subject_version_id** 等放行字段——
  public payload 只允许 target + request_id。

---

## 3. 冻结的 producer 契约（裁定 5675608420 §2-§6）

复用既有 verification 家族的三段式：密封 decision port（trusted
implementation，非 Deserialize 声明）→ 原子 decision+gap 提交 →
digest 可重放校验。

### 3.1 `GapVerificationDecision` 对象（durable evidence，schema v12）

```
gap_verification_decisions (INSERT-only + UPDATE/DELETE 拒绝触发器):
  decision_id            TEXT PK
  symbol                 TEXT NOT NULL
  period                 TEXT NOT NULL CHECK(period='day')   -- 首期仅 day
  start_date             TEXT NOT NULL
  end_date               TEXT NOT NULL                       -- start<=end
  subject_version_id     TEXT NOT NULL   -- 被核实的 published QFQ daily version
  subject_version_digest TEXT NOT NULL   -- 该版本 integrity sha256
  conclusion             TEXT NOT NULL   -- 'confirmed_missing_range' 才产 gap
  basis_kind             TEXT NOT NULL
  basis_ref              TEXT NOT NULL
  basis_sha256           TEXT NOT NULL
  verifier_id            TEXT NOT NULL   -- 服务端派生（§3.4）
  decided_at             TEXT NOT NULL
  decision_sha256        TEXT NOT NULL   -- canonical digest = proven_gaps.evidence_digest
```

字段相对第一版提案的增加（裁定 §3）：

- `subject_version_id` + `subject_version_digest` +
  `subject_adjustment='qfq'`（后者隐含于 `period='day'`+verifier 契约
  约束，或显式字段——实现期按最小字段表确定）：decision 必须绑定被
  核实的 local version，使"覆盖缺口"相对一个具体 published QFQ
  daily 版本而非抽象区间。
- `verifier_id`：服务端从已安装 verifier identity 派生，caller 不可
  提供。
- `basis_kind`/`basis_ref`/`basis_sha256`：指向底层检查依据对象。

`proven_gaps` 行绑定：`evidence_kind='gap_verification_decision'`、
`evidence_id=decision_id`、`evidence_digest=decision_sha256`。
**`gap_id` 由 decision 内容确定性派生**（domain-separated canonical
digest，如 `sha256("gap-v1" || canonical(decision_identity/content))`
），禁止随机——同 decision 重放必得同 gap_id（裁定 §5）。

### 3.2 写入时机、原子性、幂等（裁定 §3/§5）

- **写入时机**：仅 `verify_data_gap` 任务执行、installed trusted
  verifier 返回可提交 decision、且全部校验通过后写入。
- **校验链**（全部 fail-closed）：
  1. subject version 存在、为该 `symbol/day/qfq` 的 published 版本、
     完整可读、`subject_version_digest` 与记录 digest 一致；
  2. **stale guard**：decision 最终提交时，current effective daily
     head 必须**仍等于**受理时冻结的 `subject_version_id`；head 已变
     化 → fail-closed 要求重新核实，不基于旧快照创建新 open gap；
  3. basis 对象存在、`basis_sha256` 与内容一致、basis 结论为
     `confirmed_missing_range`（source-limitation 结论合法但不产
     gap）；
  4. `decision_sha256` 与 canonical 内容重算一致；
  5. `verifier_id` 等于服务端从安装 verifier 派生的身份。
- **原子性**：decision 行与对应 `proven_gaps` 行**同一写事务**提交；
  单侧存在即 integrity 违例，读路径 fail-closed。
- **幂等**：同 `decision_id` 同内容重放 → no-op；同 `decision_id`
  异内容 → integrity conflict；同 `gap_id` 异内容 → conflict（沿用
  `record_proven_gap` 既有语义）。
- **升级**：schema v12 显式 migration；旧库升级零 decision 零 gap，
  不回填、不推断。
- **存储层边界不变**：`record_proven_gap::validate()` 保持纯机械校
  验；语义校验全部在 producer 层。

### 3.3 basis 合法性（裁定 §2，OQ-1 RESOLVED）

生产可接受的 basis **只能来自安装的、受信且不可由 MCP payload 伪造
的 source-gap verifier port**（trait 实现，非 Deserialize——caller
JSON 声明不构成 verifier 输出，沿 `StopInspector` 先例）。

- caller 只能提交"核实哪个 `symbol/day/[start,end]`"+`request_id`；
  **不得**提交结论、basis_sha256、decision_sha256、decided_by、
  subject_version_id 或任何可直接放行的 evidence 字段。
- verifier 产出的 durable basis 至少绑定：
  - source/provider identity + source contract/version；
  - target `symbol + day + [start_date, end_date]`；
  - 被检查的源端事实/可用性对象或可重放 artifact identity+digest；
  - 结论必须能证明"当前 published local version 在该范围存在**应
    补**的覆盖缺口"——而非仅证明请求失败、源端空响应或源端不存在
    数据；
  - `observed_at`、verifier version、canonical digest。
- operator 文件最多只能作为 trusted verifier 的**输入 artifact**，
  不能直接作为 `confirmed_missing_range` 签发物。
- #54/#46/#71 未解除前，production verifier 必须 fail-closed（建议
  错误 `GAP_BASIS_NOT_VERIFIED`）；fixture verifier 仅用于测试。

### 3.4 `decided_by` 授权（裁定 §4，OQ-2 RESOLVED）

- `decided_by`/`verifier_id` **由服务端从已安装 verifier identity 派
  生并冻结**（verifier implementation/version + source
  contract/config identity；canonical 编码由实现文档冻结）；caller
  不可提供。
- 授权发起 ≠ 授权签发：人/代理有权发起核实任务，无权直接签发
  `confirmed_missing_range`。
- 未安装/未授权 verifier → fail-closed。

### 3.5 任务契约（裁定 §6，OQ-4 RESOLVED：契约 A）

- MCP operation：`verify_data_gap`；dataset：
  `market.gap_verification`；contract_version：
  `gap-verification-task-v1`；首期仅 `period=day`。
- public payload 仅 `request_id` + target（symbol/day/start/end）；
  无放行字段。
- application 在**首次受理快照**冻结 subject head/version identity
  （subject_version_id/digest 进 item `frozen_inputs`）；runner 调
  trusted verifier；提交 decision 前再次核对 frozen subject head 未
  变化（§3.2-2 stale guard）。
- 沿用异步任务/request_id/取消/重启/verification 既有保护；普通
  market write result 或 generic verification conclusion 不自动升级
  为 gap decision。

### 3.6 时序（裁定 §7，OQ-5 RESOLVED）

decision **不自动触发** backfill/update。它只追加 durable
proven-gap；decision 落地之后发起的下一次 default update/backfill
才在首次受理时冻结该 gap。已受理任务不扩张、不重算（保持首期"手动
触发"边界与 #4 AC-133/134/135 freeze 语义）。

---

## 4. OPEN-QUESTION 表 → 全部 RESOLVED

| # | 问题 | 裁定 | 落点 |
|---|------|------|------|
| OQ-1 | basis 合法性/来源协议 | **RESOLVED**（裁定 §2） | §3.3：仅 installed trusted source-gap verifier port；operator 文件至多是 verifier 输入 artifact；official verifier `GAP_BASIS_NOT_VERIFIED` fail-closed 至 #54/#46/#71 解除 |
| OQ-2 | `decided_by` 签发授权 | **RESOLVED**（裁定 §4） | §3.4：服务端从 verifier identity 派生，caller 不可供；未授权/未安装 fail-closed |
| OQ-3 | decision 存储形态 | **RESOLVED** 选 A（裁定 §5） | §3.1：schema v12 `gap_verification_decisions` 独立 INSERT-only 表 |
| OQ-4 | 输入契约形态 | **RESOLVED** 选契约 A（裁定 §6） | §3.5：`verify_data_gap`/`market.gap_verification`/`gap-verification-task-v1`，day only |
| OQ-5 | decision→backfill 时序 | **RESOLVED**（裁定 §7） | §3.6：不自动触发；下一次 default 受理冻结；已受理任务不扩张 |

**遗留实现期细节**（非授权问题，实现文档冻结）：`verifier_id` 的
canonical 编码（impl/version+source contract/config identity 的具体
序列化）、`gap_id` domain-separated digest 的精确拼串、`basis_kind`
枚举值集合。

---

## 5. 测试矩阵（供后续切片复用）

| 层 | 测试 |
|----|------|
| schema v12 | migration identity、INSERT-only 触发器拒 UPDATE/DELETE、旧库升级零行、无回填 |
| decision 校验 | `decision_sha256` 与内容不符 → fail-closed；缺字段/未知字段/非法区间(start>end)/非64hex digest/`period≠day` → 拒绝；非 `confirmed_missing_range` 结论（含 source-limitation）不写 `proven_gaps` |
| subject 绑定 | subject_version 非 published/不可读/digest 不符 → fail-closed；**head 在受理后漂移 → decision 提交 fail-closed（stale guard），不产生 open gap** |
| caller 放行字段 | payload 携带 conclusion/basis_sha256/decision_sha256/decided_by/subject_version_id → decode 即拒绝（deny_unknown_fields） |
| verifier 授权 | 未安装/未授权 verifier → fail-closed；`verifier_id` 与安装身份不符 → fail-closed；official verifier 返回 `GAP_BASIS_NOT_VERIFIED` |
| 原子性 | decision 成功但 gap 插入失败（注入）→ 整事务回滚无孤儿；反之亦然 |
| 幂等 | 同 decision 重放 → 同 `gap_id`（确定性派生）+ no-op；同 decision_id 异内容 → conflict；同 gap_id 异内容 → conflict |
| 禁止源回归 | task failed/cancelled/interrupted/0 行 segment/coverage_reason/'请求过范围'/`Evidence::Missing`/日期不连续/source not-available —— 全路径断言不产生 `proven_gaps` 行 |
| 端到端 | fixture verifier decision→gap→`get_data_availability` confirmed_gaps→下一次 default backfill 受理冻结该 gap→执行→`cover_gap`→open set 清空；**已受理任务不因新 decision 扩张** |
| 重放/审计 | decision 读路径 digest 重算不一致 → integrity；既有 proven_gap/covering 测试不回归 |

## 6. 实现切片顺序（裁定 §8，互不合并，各自独立评审）

1. **本文档修订**（当前）：docs-only，diff-check/CI。
2. **P1 — schema v12 decision durable fact**：`gap_verification_decisions`
   表 + adapter 读写原语 + 机械校验 + 触发器/升级测试；不接
   producer、不接 wire。
3. **P2 — trusted verifier port + internal producer**：verifier trait
   （非 Deserialize）+ decision 校验链（subject/stale/basis/digest/
   verifier_id）+ 同事务 decision+gap 写入 + 确定性 gap_id 派生 +
   fixture verifier 测试；official verifier 继续 `GAP_BASIS_NOT_VERIFIED`
   fail-closed。
4. **P3 — `verify_data_gap` public wire**：契约受理（payload 仅
   target+request_id、subject head 首次受理冻结）+ runner/worker/
   progress 投影；仍不解除 source gates。

## 7. 本设计明确不做

- 不实现任何 producer 代码、schema、wire（本 PR docs-only）；
- 不解除 #54/#46/#71，不改变 `NOT_PRODUCTION_READY`；
- 不定义 basis 的来源协议实现（official verifier 保持 fail-closed）；
- 不提议任何 generic 失败/0 行/日期不连续/自由文本/source
  not-available 触发路径；
- 不要求 `proven_gaps`/`gap_coverings`/`record_proven_gap` 语义变更；
- decision 永不自动触发 update/backfill；不删除/改写历史
  decision/gap 行。
