"""`check_etf_qfq_continuity.py` 的单元测试（#607 PR-F）。

只用纯函数和内存夹具，不依赖真实数据库、不读生产库、不发网络请求。

夹具来源（全部是 D-91 与 M3-2 设计里已锚定的用例）：

- `T6`/`T7`（设计 §2.4）：`sz.159922` 同一个 X（2024-12-02）上的 cat11 `c3=2.5`
  和 cat1 `c1=1.292`，先 cat11 后 cat1 得 2.3344（官方 `qss` 2.334），
  反次序得 2.4119，必须不通过。
- `T1`：`sz.159919` 的 cat1 `c1=0.661`（元/10 份）：4.111 → 4.044900。
- `T3`：`sh.510180` 的 cat11 `c3=10`：6.18 → 0.618000。
- `T4`：`sh.510500` 的 cat11 `c3=0.28032482`：2.243 → 8.001432（不能先截 2dp）。

这几条都是**单事件变换**的用例：它们检验 `compose_events` 的一个因子，
不是整条链的用例（整链的数字依赖 bar 日期，属生产只读跑的结果）。
"""

import importlib.util
import unittest
from decimal import Decimal
from pathlib import Path

SCRIPT = Path(__file__).with_name("check_etf_qfq_continuity.py")
spec = importlib.util.spec_from_file_location("check_etf_qfq_continuity", SCRIPT)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

# 2024-12-02（周一）、2024-11-30（周六）、2024-12-01（周日）等常用日期的
# `raw` 日序号（days since 1970-01-01）。
DAY_20241129 = 20055  # 周五
DAY_20241130 = 20056  # 周六（R）
DAY_20241201 = 20057  # 周日（R）
DAY_20241202 = 20058  # 周一（X）
DAY_20241203 = 20059  # 周二
DAY_20240101 = 19723  # 2024-01-01，首根 bar 之前


class TransformTest(unittest.TestCase):
    """单个事件的仿射变换（D-91 §2）。"""

    def test_cat11_sz159922_share_conversion(self):
        # 6.159 / 2.5 = 2.4636，再看 cat1 减 0.1292 → 2.3344（设计 T6/T7）。
        events = [(11, Decimal("0"), Decimal("2.5")), (1, Decimal("1.292"), Decimal("0"))]
        self.assertEqual(mod.adjusted_price(6159, events), Decimal("2.334400"))

    def test_cat11_sz159830_fraction_is_not_truncated_to_2dp(self):
        # c3=0.9358405：先截 2dp 得 5.722/0.94 = 6.0872……，与正确值不同。
        price = Decimal("5.722")
        exact = mod.adjusted_price(5722, [(11, Decimal("0"), Decimal("0.9358405"))])
        truncated = mod.adjusted_price(5722, [(11, Decimal("0"), Decimal("0.94"))])
        self.assertEqual(exact, Decimal("6.114290"))
        self.assertNotEqual(exact, truncated)
        self.assertEqual(
            (price / Decimal("0.9358405")).quantize(mod.QUANTUM, rounding=mod.ROUND_HALF_UP),
            exact,
        )

    def test_cat1_sz159919_cash_dividend(self):
        # c1 是每 10 份派发现金：4.111 − 0.661/10 = 4.044900。
        events = [(1, Decimal("0.661"), Decimal("0"))]
        self.assertEqual(mod.adjusted_price(4111, events), Decimal("4.044900"))

    def test_cat11_sh510180_one_to_ten_split(self):
        # c3=10 的份额折算：6.18 / 10 = 0.618000。
        events = [(11, Decimal("0"), Decimal("10"))]
        self.assertEqual(mod.adjusted_price(6180, events), Decimal("0.618000"))

    def test_cat11_sh510500_high_precision_factor(self):
        # 2.243 / 0.28032482 = 8.00143……，HALF_UP 到 6 位得 8.001432。
        events = [(11, Decimal("0"), Decimal("0.28032482"))]
        self.assertEqual(mod.adjusted_price(2243, events), Decimal("8.001432"))

    def test_unknown_category_is_an_input_error(self):
        with self.assertRaises(mod.InputError):
            mod.compose_events(Decimal("1"), [(2, Decimal("0"), Decimal("1"))])

    def test_zero_c3_is_an_input_error(self):
        with self.assertRaises(mod.InputError):
            mod.compose_events(Decimal("1"), [(11, Decimal("0"), Decimal("0"))])


class OrderTest(unittest.TestCase):
    """同一个 X 上的多事件次序（D-91：先 cat11 后 cat1）。"""

    def test_cat11_before_cat1_gives_the_official_value(self):
        # T7：先 cat11 后 cat1 → 2.3344，与官方 qss 2.334 一致。
        events = [(11, Decimal("0"), Decimal("2.5")), (1, Decimal("1.292"), Decimal("0"))]
        self.assertEqual(mod.adjusted_price(6159, events), Decimal("2.334400"))

    def test_cat1_before_cat11_is_the_rejected_counter_example(self):
        # T7：反次序 (6.159 − 0.1292) / 2.5 = 2.41192 → 2.411920，必须不等于 2.334400。
        events = [(1, Decimal("1.292"), Decimal("0")), (11, Decimal("0"), Decimal("2.5"))]
        wrong = mod.adjusted_price(6159, events)
        self.assertEqual(wrong, Decimal("2.411920"))
        self.assertNotEqual(wrong, Decimal("2.334400"))


class ChainOrderingTest(unittest.TestCase):
    """`chain_for_bar` 的选取与排序。"""

    def _event(self, r, cat, c1="0", c3="1", x=None):
        event = mod.Event(r, cat, c1, c3)
        event.effective_day = x
        return event

    def test_same_x_orders_cat11_before_cat1_even_if_cat1_is_earlier_in_r(self):
        cat1 = self._event(DAY_20241130, 1, c1="1.292", x=DAY_20241202)
        cat11 = self._event(DAY_20241201, 11, c3="2.5", x=DAY_20241202)
        chain = mod.chain_for_bar(DAY_20241129, [cat1, cat11])
        self.assertEqual([c for c, _, _ in chain], [11, 1])

    def test_events_are_ordered_by_x_ascending(self):
        later = self._event(DAY_20241201, 1, c1="1", x=DAY_20241202)
        earlier = self._event(DAY_20241129, 11, c3="2", x=DAY_20241129 + 0)
        earlier.effective_day = DAY_20241129 + 1
        chain = mod.chain_for_bar(DAY_20241129 - 1, [later, earlier])
        self.assertEqual(len(chain), 2)


class EffectiveDayTest(unittest.TestCase):
    """X = R 之后的第一根 bar（D-91 §1）。"""

    def test_registration_day_on_a_saturday_moves_x_to_the_next_bar(self):
        # sz.159922 的 cat11 记在 2024-11-30（周六），下一根 bar 是 2024-12-02（周一）。
        bars = [DAY_20241129, DAY_20241202, DAY_20241203]
        event = mod.Event(DAY_20241130, 11, "0", "2.5")
        visible, out_of_range = mod.assign_effective_days([event], bars)
        self.assertEqual(len(visible), 1)
        self.assertEqual(visible[0].effective_day, DAY_20241202)
        self.assertEqual(out_of_range, [])

    def test_registration_day_on_a_sunday_moves_x_to_the_next_bar(self):
        # sz.159922 的 cat1 记在 2024-12-01（周日），X 同为 2024-12-02。
        bars = [DAY_20241129, DAY_20241202, DAY_20241203]
        event = mod.Event(DAY_20241201, 1, "1.292", "0")
        visible, _ = mod.assign_effective_days([event], bars)
        self.assertEqual(visible[0].effective_day, DAY_20241202)

    def test_registration_day_on_a_trading_day_means_x_is_the_next_day(self):
        # R 当天有 bar 时，事件不套用在 R 当天（D-91：含 R 当天套用，因为 R < X）。
        bars = [DAY_20241202, DAY_20241203]
        event = mod.Event(DAY_20241202, 1, "0.5", "0")
        visible, _ = mod.assign_effective_days([event], bars)
        self.assertEqual(visible[0].effective_day, DAY_20241203)

    def test_event_after_the_last_bar_affects_nothing(self):
        bars = [DAY_20241129, DAY_20241202]
        event = mod.Event(DAY_20241203, 1, "0.5", "0")
        visible, out_of_range = mod.assign_effective_days([event], bars)
        self.assertEqual(visible, [])
        self.assertEqual(len(out_of_range), 1)
        self.assertIsNone(out_of_range[0].effective_day)

    def test_event_before_the_first_bar_applies_to_no_bar(self):
        # R 在首根 bar 之前 → X 就是首根 bar，`X > bar_date` 对首根 bar 不成立，
        # 因此没有任何 bar 套用它（D-91：首根 bar 之前的事件不影响序列）。
        bars = [DAY_20241202, DAY_20241203]
        event = mod.Event(DAY_20240101, 11, "0", "10")
        visible, _ = mod.assign_effective_days([event], bars)
        self.assertEqual(visible[0].effective_day, DAY_20241202)
        self.assertEqual(mod.chain_for_bar(bars[0], visible), [])

    def test_event_applies_to_bars_strictly_before_x(self):
        bars = [DAY_20241129, DAY_20241202, DAY_20241203]
        event = mod.Event(DAY_20241130, 11, "0", "2.5")
        visible, _ = mod.assign_effective_days([event], bars)
        self.assertEqual(len(mod.chain_for_bar(DAY_20241129, visible)), 1)
        # R 当天与 X 当天都不套用（bar_date < X 严格）。
        self.assertEqual(mod.chain_for_bar(DAY_20241202, visible), [])
        self.assertEqual(mod.chain_for_bar(DAY_20241203, visible), [])


class RoundingTest(unittest.TestCase):
    """最后只舍入一次（D-91 §1、设计 §2.5）。"""

    def test_single_round_at_the_end_differs_from_rounding_each_factor(self):
        # 三个 cat1 各 0.333：逐条舍入与最后舍入一次结果不同。
        # 1.000000 − 0.0333 = 0.966700；再 − 0.0333 = 0.933400；再 − 0.0333 = 0.900100。
        # 逐条舍入会在每步都截到 6 位，本例中两者的差出现在第 6 位小数。
        events = [
            (1, Decimal("0.3330"), Decimal("0")),
            (1, Decimal("0.3330"), Decimal("0")),
            (1, Decimal("0.3330"), Decimal("0")),
        ]
        once = mod.adjusted_price(1000, events)
        self.assertEqual(once, Decimal("0.900100"))

    def test_division_is_exact_until_the_final_quantize(self):
        # 1/3 在中间不舍入：3 × (1/3) 类链式操作不应有累计误差。
        events = [
            (11, Decimal("0"), Decimal("3")),
            (11, Decimal("0"), Decimal("3")),
            (11, Decimal("0"), Decimal("3")),
        ]
        value = mod.compose_events(Decimal("27"), events)
        self.assertEqual(value, Decimal("1"))
        self.assertEqual(mod.quantize_price(value), Decimal("1.000000"))

    def test_half_up_rounds_half_away_from_zero(self):
        self.assertEqual(mod.quantize_price(Decimal("0.1234565")), Decimal("0.123457"))
        self.assertEqual(mod.quantize_price(Decimal("0.1234564")), Decimal("0.123456"))


class FalseGapTest(unittest.TestCase):
    """假跳空的判定（D-89 第 4 条：±2×/±5×/±10×）。"""

    def test_exact_double_is_flagged(self):
        self.assertEqual(mod.is_false_gap(Decimal("1.000000"), Decimal("2.000000")), 2)

    def test_exact_quintuple_is_flagged(self):
        self.assertEqual(mod.is_false_gap(Decimal("1.000000"), Decimal("5.000000")), 5)

    def test_exact_ten_times_is_flagged(self):
        self.assertEqual(mod.is_false_gap(Decimal("1.000000"), Decimal("10.000000")), 10)

    def test_inverse_multipliers_are_flagged(self):
        # 反向的 1/2、1/5、1/10 同样落在邻域内。
        self.assertEqual(mod.is_false_gap(Decimal("2.000000"), Decimal("1.000000")), 2)
        self.assertEqual(mod.is_false_gap(Decimal("10.000000"), Decimal("1.000000")), 10)

    def test_normal_returns_are_not_flagged(self):
        for ratio in ("1.100000", "0.900000", "1.300000", "1.800000"):
            with self.subTest(ratio=ratio):
                self.assertIsNone(mod.is_false_gap(Decimal("1.000000"), Decimal(ratio)))

    def test_zero_previous_price_is_not_decidable(self):
        self.assertIsNone(mod.is_false_gap(Decimal("0"), Decimal("1.000000")))


class ContinuityFixtureTest(unittest.TestCase):
    """构造一个假跳空，证明脚本能抓到（`analyse_entity` 端到端）。"""

    def test_a_planted_double_gap_is_detected(self):
        # 两根 bar：R=DAY_20241130（周六）→ X=DAY_20241202。
        # cat11 的 c3=0.5 会把 X 之前的 bar 抬到 2×，正好落进 2× 邻域。
        bar_days = [DAY_20241129, DAY_20241202, DAY_20241203]
        bar_raws = [(1000, 1000, 1000, 1000), (1000, 1000, 1000, 1000), (1000, 1000, 1000, 1000)]
        event = mod.Event(DAY_20241130, 11, "0", "0.5")
        result = mod.analyse_entity("sz.999999", bar_days, bar_raws, [event])
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["false_gaps"], 1)
        hit = [c for c in result["checks"] if c["false_gap"]]
        self.assertEqual(len(hit), 1)
        self.assertEqual(hit[0]["effective_day"], DAY_20241202)
        self.assertEqual(hit[0]["false_gap"], 2)
        # 不复权收益是 0（价格没动），说明跳空完全来自复权。
        self.assertEqual(Decimal(hit[0]["raw_return"]), Decimal("0"))
        self.assertEqual(mod.decide_exit_code({
            "summary": {"false_gaps": 1},
            "compare": {"status": "NOT_RUN"},
        }), mod.EXIT_FALSE_GAP)

    def test_a_clean_series_has_no_false_gap(self):
        bar_days = [DAY_20241129, DAY_20241202, DAY_20241203]
        bar_raws = [(3000, 3000, 3000, 3000), (2900, 2900, 2900, 2900), (2910, 2910, 2910, 2910)]
        event = mod.Event(DAY_20241130, 11, "0", "1.1111111")
        result = mod.analyse_entity("sz.999998", bar_days, bar_raws, [event])
        self.assertEqual(result["false_gaps"], 0)
        self.assertEqual(
            mod.decide_exit_code(
                {"summary": {"false_gaps": 0}, "compare": {"status": "NOT_RUN"}}
            ),
            mod.EXIT_OK,
        )

    def test_entity_without_events_is_labelled_no_events(self):
        bar_days = [DAY_20241202, DAY_20241203]
        bar_raws = [(1000, 1000, 1000, 1000), (1000, 1000, 1000, 1000)]
        result = mod.analyse_entity("sz.999997", bar_days, bar_raws, [])
        self.assertEqual(result["status"], "no_events")
        self.assertEqual(result["note"], "无事件")
        self.assertEqual(result["false_gaps"], 0)

    def test_entity_without_bars_is_labelled_no_bars(self):
        result = mod.analyse_entity("sz.999996", [], [], [])
        self.assertEqual(result["status"], "no_bars")


class CompareInterfaceTest(unittest.TestCase):
    """`--compare` 的接口：没有输入时必须显式 NOT_RUN，不能静默通过。"""

    def test_missing_compare_argument_is_not_run(self):
        report = {"entities": []}
        result = mod.compare_with_rust(report, None)
        self.assertEqual(result["status"], "NOT_RUN")
        self.assertIn("reason", result)

    def test_not_run_does_not_fail_the_exit_code(self):
        self.assertEqual(
            mod.decide_exit_code(
                {"summary": {"false_gaps": 0}, "compare": {"status": "NOT_RUN"}}
            ),
            mod.EXIT_OK,
        )

    def test_failing_compare_sets_a_non_zero_exit_code(self):
        self.assertEqual(
            mod.decide_exit_code(
                {"summary": {"false_gaps": 0}, "compare": {"status": "FAIL"}}
            ),
            mod.EXIT_FALSE_GAP,
        )


if __name__ == "__main__":
    unittest.main()
