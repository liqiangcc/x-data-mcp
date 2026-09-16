# issue-46-ashare-contract — 沪深A 目录组合契约 + AC 机械验收

refs #46（FG ruling comment-5690659606 解除字段语义外部 blocker 后的剩余验收切片）。上游证据：#161 字段级盘点、#164 锚定绑定、#167 Windows 行为。

## 交付物

- `ashare-directory-contract-v1.md` — 组合契约草案（范围/身份/名称/研究行业二级/快照保护/UNANCHORED 清单/给 #6 的可实施-阻塞表）。
- `plan.json` — 冻结计划：输入 pin、锚定语义、范围规则、固定样本、AC 预期。
- `scan_ashare_contract.py` — 离线 scanner，对 gen-2 retained 成员逐 AC 机械验证。
- `result.json` — 20 checks 全 ok；`ac_verdicts` 逐条判定。
- `determinism.json` — 双跑 byte-identical 证据。
- `test_scan_ashare_contract.py` — 解析器单测 + result 一致性检查。

## 复现

```sh
python3 scan_ashare_contract.py            # 重生成 result.json
python3 scan_ashare_contract.py --check result.json
python3 -m unittest test_scan_ashare_contract -v
```

输入：`/home/box/retained-inputs/` gen-2 归档（pin 见 plan.json）+ 仓库内 `binding.json` / #167 observations。无网络。
