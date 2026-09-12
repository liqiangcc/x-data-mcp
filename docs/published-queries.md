# 已发布行情查询（Issue #68）

三个新工具只读状态，不启动采集、补采、修复或发布。数据来自带完整性摘要的已发布存储；摘要不是官方来源认证。生产仍 NOT_PRODUCTION_READY，live source NOT_RUN。

`get_market_data` 必填 exchange（sh/sz）、code（六位）、period（day/year）、start_date/end_date（YYYY-MM-DD）。存储身份为 exchange.code，不猜测旧的纯代码属于哪个交易所。可选 version_id；省略时新查询取最后提交版本，显式历史版本不存在或损坏均拒绝，不回退。日线日期闭区间升序；年线按起止自然年匹配完整已发布年度记录，不按月份截取。

`list_market_data_versions` 必填 exchange/code/period，返回可核验已发布版本，按 published_at 降序、version_id 升序。任一列表记录损坏时列表拒绝，另一个完整历史版本仍可显式查询。

两个工具支持 page_size（默认100，1至1000）及 cursor；后续请求重复原条件。分页固定版本/列表、条件和 query_time。每页重新核验固定记录，文件损坏或同id内容替换即拒绝，不切到新版本。新增版本不影响已有分页。next_cursor=null、page_complete=true仅表示结果取完，不表示数据完整。

Cursor是当前进程内凭据，缓存最多64个后续页状态；超过后淘汰最早凭据，重启失效。无效/过期/条件不同均要求新查询。页大小与缓存数量为工程上限，不是性能承诺。当前存储及分页会在内存中加载版本，尚无全市场规模性能证据。

返回保存的Decimal值和单位/币种证据，不换算或填零。first_record_date/last_record_date是实际保存边界，不代表交易日覆盖；coverage和缺失原因暂为UNKNOWN，freshness为NOT_VERIFIED，不把published_at或query_time当行情截至日期。future_interval_not_occurred指请求包含查询时点以后日期，不能解释为历史缺口。完全无数据返回空records与null版本。

年线必须带local-natural-year-v1 lineage，核验固定已发布daily、来源/单位及精确聚合相符后，返回已发布年线对应的bar与逐年lineage、year_unfinished。它表示derivation_as_of（发布日的上海日期）时该年度是否未结束，不暗示跨年后旧年线自动变完整。参与首末日来自所选自然年，可以位于跨年daily版本内部。此处重算仅验证保存结果，不生成或发布未保存的年线。缺失/未知算法或不一致明确不可用。

`get_request`必填request_id，返回bound与实际task状态；未绑定返回false，I/O失败报错。旧不完整绑定返回request_needs_verification，不伪装为pending。此查询不修复状态。父#5的全部任务进度/核实管理能力不因本子任务完成而关闭。

所有工具包括既有工具拒绝未知参数；错误参数无文件写副作用。stdio仅在授权本机进程边界使用，不开放HTTP；Cursor不是认证。部署的独占写入、访问控制与真实来源验收仍需独立交付。
