# #6 证券目录实现设计：官方文件 → `securities.catalog`（设计冻结）

状态：设计冻结，供协调决策/Final Gate 评审。**本文件不落代码、不改
schema/wire、不创建任何新运行时路径；生产来源继续
`NOT_PRODUCTION_READY`，#71 文件获取/使用边界门禁不因本设计解除。**

依据（均已落 main）：

| 输入 | 身份 |
|------|------|
| #168 契约 v1 `evidence/issue-46-ashare-contract/ashare-directory-contract-v1.md` | main `d2c0248`；S=5230、字段语义、排除枚举、保护规则、UNANCHORED 清单的唯一权威来源 |
| #46 completed（FG handoff） | #6 评论 2026-09-16：两项显式产品裁定移交本 Issue |
| #6 Issue 本体 | AC-01..AC-12、最小字段、刷新/消失/冲突保护已确认规则 |
| #155 设计冻结先例 | `docs/tasks/issue-155-gap-producer.md` 的格式与"设计≠授权"纪律 |

基线：`origin/main` = `1226c51`；分支 `issue-6-catalog-design`，worktree
`/home/box/x-data-mcp-tdxhy46`。

---

## 0. 契约输入摘要（#168 逐字承接，不加推断）

| 输入 member | 所属 retained 输入 | 用途 |
|---|---|---|
| `official-base.zip!tdxhy.cfg` | gen-2 `dcdf6b49…`（与 #167 Windows V7.73 live 拷贝字节一致） | 范围 universe：f0 交易所 / f1 代码 / f2 T-code / f5 X-code（f3/f4 本代恒空） |
| `official-base.zip!infoharbor_ex.code` | 同上 | 名称 join：`f0|f1|keywords`（GB18030） |
| `official-gbbq.zip!zhb.zip!incon.dat` | gen-2 `daad6290…` | `#TDXRSHY` X 码→名称字典（474 条，level=(len-1)//2） |
| `binding.json`（#164, sha `67508fca…`） | 已提交派生证据 | X 层级断言与 PDF 声明层级的复核依据 |

**范围 S（已锚定）**：`f0 ∈ {0,1} 且 f2 非空 且 f5 非空` → 本代 5230 成员。
排除集（必须显式枚举、不得静默丢弃）：BJ `f0=2` 348 行；空 T/X 行 81 条 =
B 股 `200xxx/900xxx` 78 条 + 异常码 `002710/002720/201872` 3 条。
行数对账恒等式：`总行数 = |S| + |BJ| + |空 T/X|`，本代 5659 = 5230 + 348 + 81。

gen-2 f5 长度分布：5230 成员中 len(f5) ∈ {5,7}（L2 直连 764、L3 leaf
4466），`f5[:5]` 对全部 S 成员可派生且 ∈ TDXRSHY level2。

---

## 1. 现有实现盘点：复用 / 扩展 / 新增逐项判定

| 结构 | 位置 | 现状 | 判定 |
|---|---|---|---|
| `SecurityRecord`/`SecurityKey`/`Exchange`/`SecurityType::AShare` | `catalog.rs` | 字段已覆盖最小契约（symbol/name/exchange/security_type/source_security_id/industry/source/acquired_at） | **复用**；`IndustryInfo` 需扩展（§5-P2） |
| `IndustryInfo::{Assigned,Missing,Conflicted}` | `catalog.rs` | AC-05/AC-11 状态枚举已存在 | **扩展**（scheme 标签，见 §6） |
| `CatalogSnapshot`/`CompletenessEvidence` | `catalog.rs` | 自报完整性证据 | **复用**，语义收紧见 §4.2 |
| `CatalogService::refresh` + `validate_snapshot` | `catalog.rs` | AC-04/06/07/08/09/10 判定逻辑已全部实现：source 非空、expected_count、EmptySnapshot、InvalidRecord、DuplicateSecurity、IdentityConflict、MemberDisappeared | **复用**为唯一校验入口；不得绕过 |
| `JsonCatalogStore` | `catalog.rs` | 文件 JSON 仓 | **复用为测试/fixture 路径**；生产走 SQLite |
| `catalog_versions`/`catalog_members`/`catalog_head` + `read()` | `sqlite_catalog.rs`, migration 004 | digest + 成员投影 + `validate_snapshot` 三重读回复核 | **复用**；读路径零改动 |
| `SqliteCatalog::publish_fixture` | `sqlite_catalog.rs` | 仅 `fixture:` 前缀、≤10 万行、expected-head 乐观锁、单事务 versions+members+head | **新增** `publish_official`：同事务同校验，去 fixture 门禁，加输入绑定（§4.3） |
| `SnapshotManifest`/`FileRole::BaseIndustry`/`inspect()` | `snapshot.rs` | base.zip 角色→文件名→URL→sha256 绑定已冻结（preview.8/9 验收） | **复用**为官方输入入口；目录构建必须消费已 inspect 的快照 |
| `securities.catalog` dataset + `CatalogScope::CurrentShSzA` | `data_supply/wire.rs` | wire 契约已存在 | **复用**；不加新枚举 |
| catalog collection → `catalog:current_sh_sz_a` 单 item | `data_supply/collect.rs` | FrozenWork/ItemSpec 已就绪 | **复用** |
| `BlockedSource` worker → `SOURCE_NOT_VERIFIED` | `official_files.rs` | 当前全部 catalog item 拒绝 | **新增** `TdxCatalogSource` 替换该 item 的执行器；`CATALOG_INDUSTRY_NOT_VERIFIED` blocker 条目需随真实接入评审更新 |
| `default_selection` / `WriteSelection::Catalog{version_id,industry_code}` | `data_supply/*` | 按 `industry.code==industry_code` 过滤目录成员 | **复用**；`industry_code` 语义=TDX 研究行业 L2 码（X 5 位），须在使用处显式标注 scheme（§5-P2） |
| `dbf.rs` | `src/dbf.rs` | base.dbf 保守容器解析 | **不使用**：契约 §4 明示 base.dbf 全部字段 UNANCHORED；目录实现不依赖 |
| tdxhy.cfg / ex.code / incon.dat 解析 | 无 | 不存在 | **新增**三个 bounded 解析器（§2），沿用 `dbf.rs` 风格：结构校验+原始字段保留，不猜编码不补语义 |

**结论**：存储、发布保护、wire、受理路径全部复用既有结构；新增面收敛为
三个解析器 + 一个 source 实现 + 一个生产发布方法 + 一处 schema 扩展。

---

## 2. 数据流（逐段机械，每段自带失败类）

```
SnapshotManifest inspect (snapshot.rs, 既有)
  └─ S1 member 提取：base.zip!{tdxhy.cfg, infoharbor_ex.code}
                    gbbq.zip!zhb.zip!incon.dat            ← 嵌套 zip
  └─ S2 tdxhy.cfg 行解析：'|' 分隔 6 字段, ASCII, CRLF, 5659 行
  └─ S3 范围过滤：S 成员 5230 + 排除枚举（§3）
  └─ S4 名称 join：ex.code f0(code)→f1(name), GB18030 decode
  └─ S5 行业解析：f5[:5] → TDXRSHY L2 code→name
  └─ S6 CatalogSnapshot 组装（字段映射 §2.5）
  └─ S7 CatalogService::refresh → validate_snapshot → publish_official
```

### S1 成员提取（新增 `TdxMemberReader`）

- 输入 = `inspect()` 通过的 manifest + 三个 role 的本地路径；
  manifest sha256 即输入身份（§4.3）。
- 只读、不解压到磁盘以外、不执行任何包内内容；嵌套 `zhb.zip` 按字节
  流处理。
- member 缺席 / zip 损坏 / 非 UTF 路径 → `CatalogSourceError::Unavailable`
  （映射 `RejectReason::SourceFailure`，AC-04/07 保护生效）。
- member sha256 逐文件重算并与 manifest 比对——与 `inspect()` 同一
  digest 纪律，不做第二次下载。

### S2 tdxhy.cfg 解析（新增）

- 行格式冻结：`<f0>|<f1>|<f2>|<f3>|<f4>|<f5>`，ASCII，`\r\n`。
- 机械校验：恰好 6 字段；f0 ∈ {0,1,2} 单字符；f1 恰 6 位 ASCII 数字；
  f2 匹配 `T[0-9]{4,6}` 或空；f5 匹配 `X[0-9]{2,6}` 或空；f3/f4 必须为空
  （非空 → 解析失败，不赋语义）。
- 任何不合形行 → 整个 snapshot 解析失败（fail-closed），不跳行、不修复。
- f1 保留前导零，**绝不**转整数。

### S3 范围过滤 + 排除枚举（§3 形式化）

见 §3；输出 = `Vec<ScopedRow>` + `Vec<ExcludedRow{code, class}>`，
两类合计必须等于输入行数（对账恒等式，不等 → 解析失败）。

### S4 名称 join（新增）

- ex.code 行 `<code>|<name>|<keywords>`，GB18030 解码；keywords 字段
  忽略（#168 明示其角色 UNANCHORED，仅跳过不解释）。
- join 键 = f1（6 位代码）。**注意 ex.code 无交易所前缀**：同名代码在
  不同交易所的歧义由 S 成员本身消化——ex.code 与 tdxhy.cfg 的 code
  集合关系已在 #46 证据中验证（ex.code ⊆ tdxhy.cfg 码集且每码至多一
  行）；若某 code 在 ex.code 出现多行 → `IdentityConflict`（AC-10）。
- 缺名成员 → 按 §5-P1 未决策略处理，**默认 fail-closed**。

### S5 行业解析（新增）

- `len(f5) < 5`（L1 码直接出现在 f5）→ 契约外形变：本代不存在，
  出现即 fail-closed 报 `INDUSTRY_LEVEL_NOT_RESOLVABLE`，不降级猜测。
- `l2 = f5[..5]`；`l2` 必须存在于 incon.dat `#TDXRSHY` 且解析 level==2；
  否则 `IndustryInfo::Missing{reason}`（AC-05）还是解析失败？——按
  契约 §2 的机械断言，本代 5230/5230 可解析；**未来快照中不可解析的
  L2 码属"行业暂时获取不到"**，走 `IndustryInfo::Missing`（保留成员），
  不整批拒绝——与 AC-05 已确认语义一致。
- T-code（f2）为并行旧分类，**不进入** `industry_code`；`688688` 的
  `T00` 退化值如实保留在 provenance，不绑定名称。
- `Conflicted` 枚举：单一快照内 X→name 唯一，不产生冲突；保留给
  未来多来源合并裁定（AC-11），本实现不产生。

### S6 CatalogSnapshot 字段映射

| SecurityRecord 字段 | 来源 | 说明 |
|---|---|---|
| `symbol` | f1 | 6 位，前导零保留 |
| `exchange` | f0 | `0→Shenzhen, 1→Shanghai`（注意 Dict.html 枚举方向，逐行断言不复用代码前缀猜测） |
| `security_type` | — | 恒 `AShare`（S 范围已排除非 A） |
| `name` | ex.code f1 | GB18030；缺名按 §5-P1 |
| `source_security_id` | 派生 | `tdxhy:{f0}:{f1}` 确定性拼串——官方文件内身份即 (exchange,code)，不引入额外 ID 命名空间 |
| `industry` | f5[:5] + TDXRSHY | `Assigned{scheme, code:"X····", name}` 或 `Missing` |
| `source` | 派生 | `official-file:<manifest_sha256[:16]>`——绑定整个已 inspect 快照 |
| `acquired_at` | manifest `acquisition_completed_at` | 获取时间，**不冒充行业生效时间**（#6 明示） |
| `completeness` | 派生 | `Complete{expected_count = |S|}`，语义见 §4.2 |

### S7 发布

`CatalogService::refresh` 现有流程不变；repository 端点由
`JsonCatalogStore`（测试）/ `publish_official`（生产，§4.3）承担。

---

## 3. 范围规则与排除枚举形式化

```
S      = { row | f0 ∈ {0,1} ∧ f2 ≠ ε ∧ f5 ≠ ε }
E_BJ   = { row | f0 = 2 }                                    # 348
E_BSH  = { row | f0 ∈ {0,1} ∧ (f2 = ε ∨ f5 = ε) ∧ f1 ∈ 200xxx|900xxx }  # 78
E_ANOM = { row | f0 ∈ {0,1} ∧ (f2 = ε ∨ f5 = ε) ∧ f1 ∉ B 股段 }          # 3: 002710/002720/201872
不变式  = rows == S ⊔ E_BJ ⊔ E_BSH ⊔ E_ANOM  （互斥且穷尽；违例 → 解析失败）
```

- 排除行逐条记录 `{f0, f1, class}` 进 refresh 诊断输出（不落库为成员）；
  类别计数与 gen-2 基线（348/78/3）不符时**不失败但如实报告差异**——
  计数漂移是快照代际事实，不是错误；类别**语义**（BJ=B 股段）保持
  UNANCHORED 标注，仅作诊断展示。
- `E_ANOM` 逐条列出（code + 空字段形态），沿用 #168 残差呈现方式。

---

## 4. 保护规则 → 现有 schema/发布路径映射

### 4.1 AC → 现有机制映射

| #6 规则 | 现状 | 本设计落点 |
|---|---|---|
| AC-04 刷新失败保旧 | `CatalogService::rejected` + head 不动 | 复用；所有 S1–S6 失败均走 `SourceFailure`/`Incomplete`/`InvalidRecord` |
| AC-06 校验通过发布新目录 | `publish_atomic` 语义 | `publish_official`（§4.3） |
| AC-07 首次失败未就绪 | `RefreshStatus::NotReady`（previous=None） | 复用 |
| AC-08 成员消失拒绝 | `MemberDisappeared`（逐键比较，新增不抵消） | 复用 |
| AC-09 分页/重复/总数异常 | `DuplicateSecurity`/`expected_count`/`EmptySnapshot` | 文件源无分页；映射为 行数对账恒等式 + 重复 (f0,f1) 拒绝 + 空 S 拒绝 |
| AC-10 身份/来源/必填冲突 | `IdentityConflict`/`InvalidRecord` | 复用 + ex.code 一码多名 → IdentityConflict |
| AC-05 行业缺失保留 | `IndustryInfo::Missing` | S5 L2 不可解析路径 |
| AC-11 行业冲突降级 | `IndustryInfo::Conflicted` | 单来源不产生；保留枚举 |

### 4.2 `completeness` 语义收紧

`Complete{expected_count}` 对文件源的语义 = "**tdxhy.cfg 全部行已被解析
且 S+排除集对账穷尽**"，expected_count = 实际 S 成员数。它**不**断言
"覆盖全市场"或"当前上市完整性"（#168 §6 明示该断言仍阻塞）。
`Incomplete`/`Unknown` 在本实现中只能由 S1/S2 失败产生，且失败即拒绝，
故完整性与有效性不可分离——保留既有语义，不新增 evidence 类型。

### 4.3 `publish_official`（新增，与 `publish_fixture` 对偶）

- 同一事务：`catalog_versions` INSERT + `catalog_members` 投影 INSERT +
  `catalog_head` upsert；同事务内 `read()` 复核后才 commit。
- 差异点：不要求 `fixture:` 前缀；要求 `source` 匹配
  `official-file:<sha16>` 且携带 input manifest 绑定（§6）；`expected`
  head 检查保留（并发双刷新乐观锁）。
- 快照漂移纪律：同一 manifest_sha256 重跑 → 幂等（同 records 同
  source_security_id → 发布等价新版本或 no-op，由 refresh 结果决定）；
  manifest 变更即新快照代际，成员消失保护照旧适用（AC-08 不因"官方新
  一代"豁免——真实退市导致的成员缺席同样暂停发布，等核实放行）。

---

## 5. 未决产品参数（两项，显式建模，默认 fail-closed）

### P1 缺名成员处置 `name_missing_policy` — **UNDECIDED**

背景：S 成员 `001235/300728/601206/603302/603361/688688` 在 base.zip
全部成员中无名称行（#168 AC-02 PARTIAL）。策略枚举：

| 枚举 | 语义 | 实现代价 |
|---|---|---|
| `reject_candidate`（**默认**） | 任一 S 成员缺名 → `RejectReason::NameMissing{members}`，整候选拒绝，保旧目录 | 新增 RejectReason 变体；零 schema 变更 |
| `keep_with_name_missing` | 保留成员，`name` 字段空缺 + 显式 `name_status=name_missing` | `name` 现为非空必填 → 需 `Option<String>`/状态列（schema 变更）+ `validate_record` 放行 + 消费侧（default_selection/mcp 投影）对无名成员的行为契约 |
| `alt_name_source` | 采用另一**已授权且有独立身份绑定**的名称来源 | 需该来源先完成 #46 级锚定证据；当前无候选 |

**设计冻结默认值 = `reject_candidate`**：不猜名、不按代码规律补齐、不
静默放行。`keep_with_name_missing` 的实现路径在此文档预留（§6 预留列
位），但默认分支不得提前实现其语义。

### P2 行业分类标准 `industry_scheme` — **UNDECIDED**

背景：官方文件可机械提供 **TDX 研究行业二级**（5230/5230）；#6 历史需
求曾确认"东财二级"。在 #6 裁定前：

- 字段命名/存储一律携带 `tdx_research_industry` 语义标签：
  `IndustryInfo::Assigned{scheme: "tdx_research_industry", code, name}`，
  SQL 投影 `industry_scheme` 列（§6）。**任何消费侧不得把
  `industry_code` 当作东财代码使用**；`WriteSelection::Catalog` 的
  `industry_code` 过滤入参在文档/错误信息中标注 scheme。
- 是否切换到东财二级、或双轨并存，留 #6 裁定；本设计只保证切换点收敛
  在 scheme 枚举值 + 一个 source 实现分支，不散落。

---

## 6. schema / migration 草案（v13，不提交实现）

当前 `SCHEMA_VERSION=12`（012 = gap_verification_decisions）。提议
`migrations/tasks/013_catalog_official.sql`：

```sql
ALTER TABLE catalog_members ADD COLUMN industry_scheme TEXT
  CHECK(industry_scheme IS NULL OR industry_scheme='tdx_research_industry');
ALTER TABLE catalog_versions ADD COLUMN input_manifest_sha256 TEXT;
```

- `industry_scheme`：nullable 以兼容既有 fixture 行（NULL=fixture，
  不赋 scheme）；生产行恒 `'tdx_research_industry'`。
- `input_manifest_sha256`：版本 → 官方快照 manifest digest 绑定；
  fixture 行 NULL。读路径在 `read()` 投影复核中新增该列比对。
- `compat_json` 侧：`IndustryInfo::Assigned` 增加 `scheme` 字段需 serde
  兼容策略（旧 payload 无该字段 → 反序列化视为 fixture/未知 scheme，
  不静默补值）。
- 备选零迁移方案：scheme 编码进 `industry_code` 前缀
  （`tdx_research:X4202`）。否决理由：污染码值命名空间，消费侧按
  `==` 过滤会静默失配。
- 旧库升级：零行回填；既有 fixture 行 scheme=NULL 如实呈现。
- **migration 是否真正需要** 由 P1 裁定决定（`keep_with_name_missing`
  需 `name` 可空或 `name_status` 列）；本草案按最小面提出，评审可砍。

---

## 7. AC 映射表 + 验收方式

| AC | 设计落点 | 验收方式 | 残余门禁 |
|---|---|---|---|
| AC-01 范围清单 | §3 范围+排除枚举 | 单测：合成 tdxhy.cfg 断言 S/E 对账恒等式与逐条枚举 | "当前上市完整性"仍 UNANCHORED |
| AC-02 行业归属 | §2-S5 + §5-P2 | 单测：L2 派生+字典解析+scheme 标签；缺名 6 成员按 P1 默认拒绝 | 分类标准待 #6 裁定 |
| AC-03 后续采集读取 | `WriteSelection::Catalog`/default_selection 复用 | 现有消费侧测试不回归 + industry_code=tdx L2 语义标注 | — |
| AC-04 失败保旧 | `CatalogService::rejected` 复用 | 单测：S1–S6 各失败类注入 → Rejected+旧 head | — |
| AC-05 行业缺失保留 | S5 `Missing` 路径 | 单测：L2 不在字典 → Missing{reason}+成员保留 | — |
| AC-06 成功发布 | `publish_official` 同事务提交 | 单测：fixture 快照→发布→read()复核→head 推进 | — |
| AC-07 首次失败未就绪 | `NotReady` 复用 | 单测：空库+fetch 失败 → NotReady 无 head | — |
| AC-08 成员消失拒绝 | `MemberDisappeared` 复用 | 单测：快照 B 缺成员 → 拒绝+差异列表 | 真实退市放行流程未定（沿用既有待明确项） |
| AC-09 完整性异常 | 对账恒等式+dup/empty 拒绝 | 单测：缺行/重复 (f0,f1)/S=0 → 拒绝 | — |
| AC-10 冲突拒绝 | `IdentityConflict` 复用+ex.code 多行 | 单测：一码多名/字段不合形 → 拒绝 | — |
| AC-11 行业冲突降级 | `Conflicted` 保留 | 本实现不产生；枚举存在性测试 | 多来源合并待裁定 |
| AC-12 真实运行闭环 | 不在本切片 | — | **仍 NOT RUN**：需 #71 边界、P1/P2 裁定、真实快照接入验收 |

## 8. 测试矩阵（实现期复用）

| 层 | 测试 |
|---|---|
| 解析器 | 6 字段不合形/f0 越界/f1 非 6 位/f5 形态错/f3f4 非空/CRLF 缺失/GB18030 非法字节 → 全 fail-closed；前导零保留 |
| 范围 | S/E_BJ/E_BSH/E_ANOM 互斥穷尽；对账违例 → 拒绝；gen-2 基线计数差异如实报告不失败 |
| 名称 join | 缺名 → P1 默认 `reject_candidate`；一码多名 → IdentityConflict |
| 行业 | len(f5)<5 → `INDUSTRY_LEVEL_NOT_RESOLVABLE` 拒绝；L2 不在 TDXRSHY → Missing 保留；scheme 标签恒 `tdx_research_industry` |
| 发布 | manifest sha 绑定进版本；同快照幂等；expected-head 冲突 → Conflict；成员消失 → MemberDisappeared |
| 读路径 | digest/投影/scheme 列复核；fixture 行 scheme=NULL 兼容 |
| 门禁回归 | `BlockedSource` 对非 catalog item 行为不变；`NOT_PRODUCTION_READY` 字符串不删 |

## 9. 实现切片顺序（各自独立评审，互不合并）

1. **本文档**（当前）：docs-only。
2. **P1 — 解析器**：tdxhy/ex.code/incon.dat 三个 bounded 解析器 +
   范围/排除对账 + 单测（纯函数，不接存储）。
3. **P2 — schema v13 + publish_official**：迁移、输入绑定、生产发布
   方法、读路径投影扩展。
4. **P3 — TdxCatalogSource + worker 接线**：`CatalogService::refresh`
   生产装配、`catalog:current_sh_sz_a` item 执行器替换、provenance
   诊断输出。**两项产品裁定未落地前 P3 不得合入**——默认
   `reject_candidate` 虽然 fail-closed，但缺名拒绝语义本身是待裁定
   行为面。

## 10. 本设计明确不做

- 不写任何实现代码（docs-only PR）；
- 不解除 `CATALOG_INDUSTRY_NOT_VERIFIED` / `NOT_PRODUCTION_READY` /
  #71 门禁；
- 不实现两项产品裁定的任一分支语义（仅建模参数与默认 fail-closed）；
- 不使用 base.dbf/tdxhy2.cfg/ex.name 任何字段（UNANCHORED）；
- 不承诺"当前上市完整性"、退市自动识别、历史时点目录查询；
- 不引入东财/第三方分类映射；不把 TDX 研究行业与东财行业混同；
- 不做自动调度刷新（#6 已确认首期手动触发）；decision/发布不自动
  触发下游采集。
