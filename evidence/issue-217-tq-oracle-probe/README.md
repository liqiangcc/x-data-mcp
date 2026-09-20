# issue-217-tq-oracle-probe

#217 前置能力探针：验证官方 TdxQuant 接口在本机是否可调用，并留存第一批原始 oracle 输出。
这是有界能力验证，不是正式 gen-3 采集；正式采集按 docs/tasks/issue-217-gen3-acquisition.md 执行。

## 环境

- 通达信金融终端 V7.73，`C:\new_tdx64\TdxW.exe`（hash 见 issue-166 inputs.json）
- `PYPlugins/sys/tqcenter.py` v1.0.16 (2026-07-24) 包装 `TPythClient.dll`
- 客户端运行中且已登录（本次登录经由游客/重试路径进入行情页；账号级登录当时报"校验用户失败"）
- 调用方式：外部 Python 3.10.9 → `tq.initialize(<PYPlugins/user 下策略文件路径>)` → `dll.GetTdxDataStr` 原始 JSON

## 采集内容（manifest.json 绑定 sha256）

- `raw/divid_*.json` — GetTdxDataStr type=4 原始响应，7 只证券（600006.SH/600004.SH/000002.SZ/000004.SZ/600000.SH/000001.SZ/688318.SH）
- `raw/kline_*.json` — GetTdxDataStr type=1 原始响应，period=1d，dividend_type=none/front

## 发现（机械事实）

1. `get_divid_factors` 返回 `{"Date":[...], "Type":[...], "Value":[[Bonus,AllotPrice,ShareBonus,Allotment]]}`——
   Value 顺序与 gen-2 gbbq 记录 C1..C4 一一对应；服务值为**十进制字符串显示精度**
   （000002@2000-01-10 → `['0.00','7.50','0.00','2.73']`，存储 C4=f32 2.727；
   2019-08-15 Bonus='10.45'，存储 C1=f32 10.45102024078369140625）
2. `get_market_data dividend_type='none'` 返回原始 OHLC + `ForwardFactor`：
   FF_t = Π_{event>t}(ref_i/pc_i)（乘性链，全局锚定于最新事件，6dp 服务值）
3. `dividend_type='front'` 返回**以响应末根K线为锚**的前复权序列——窗口请求结果随请求范围变化；
   全历史请求与 K线前复权完全一致（含负价格 -8.06，与 #166 截图逐字段一致）
4. 000004.SZ 返回 0 事件（退市缺席，与 #166 tdxhy.cfg 缺席一致）
5. FF 链在配股事件上更贴合**存储精度**输入（2000-01-10 隐含步长 0.931968 vs stored-fit 0.931956
   vs served-2dp-fit 0.931898）——服务端因子链与客户端显示链的输入精度可能不同，如实记录

## 边界

- 仅调用官方公开接口，凭据不落库；不含逆向后门手段
- oracle 服务值是对候选规则的判别输入，不等同于业务真值的法律含义
