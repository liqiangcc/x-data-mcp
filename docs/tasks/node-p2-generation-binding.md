# Node 迁移 P2 — 代际绑定判定（generation binding）

任务：NM-031（agent-coordination/node-migration）。日期：2026-09-23。
基线：`40440f99b2354e12456b9f97042e019947e96a5f`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；#260 代际绑定来源不变式；`src/data_supply/generation.rs::{binding_of,uniform_binding}`、`tests/data_supply_generation.rs`。
证据：`evidence/node-migration/p2/generation_binding/`（probe-results.jsonl 44 行全 PASS、rust-oracle.jsonl、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果（R2）：44/44 PASS / 0 FAIL —— 21 例 live Rust oracle 全一致（含 `Ok("")` 与非对象 frozen_inputs 语义）。**

## 1. 移植：`src_node/governance/generation_binding.mjs`

- `uniformBinding(items)` → `{ok:true, generation_id}` 或 `{ok:false, refusal:"NoItems"|"Missing"|"Null"|"Mixed"}`
- `binding_of`：`frozen_inputs` 非对象（Value 上 `.get` 非对象返回 None）或缺键 → Missing 类；键存在非字符串 → Null 类；字符串 → id
- 混合判定先于首元素解释（任一 binding != first → Mixed）
- `Ok("")` 可达——`Store::usable` 是独立的后续门，纯契约不变（任务明确要求）
- 仅 own-property 查找；own-data-descriptor 校验——getter/setter/symbol/非枚举属性在**不调用**前提下拒收（副作用计数器证明零调用），自定义 prototype 同理；数组边界同样按描述符检查（R2：`v.every` 会触发索引 getter，已改逐描述符） → `GenerationBindingInputError`

## 2. 纯净性

确定性、只读；无 active 指针、fallback root、代际打开、文件钉住、存储核验或可用来源声明。

## 3. 验证矩阵（34 例）

| 组 | 例数 | 结果 |
|---|---|---|
| oracle（空/缺/null/混合/排列/非对象 frozen/unicode/空串） | 21 | PASS |
| 类型边界（非数组/非对象/缺 frozen/NaN/函数/代理对/类实例） | 9 | PASS |
| prototype 安全（继承绑定不可见/dunder/null-proto/constructor） | 4 | PASS |
| 访问器描述符（getter/setter/symbol/非枚举/数组索引，零调用证明） | 10 | PASS |
| 自测 `binding` | exit 1 | |

## 4. 范围边界

不含 store 集成、任务调度、服务/MCP、部署。

## 5. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/generation_binding_probe.mjs .` | 0（44 PASS） |
| `node ... binding`（自测负证） | 1 |
| `rustc` bind_oracle | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
