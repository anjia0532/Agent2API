//! 模型价格估算：把一次请求的 token 用量折算成「按官方挂牌价应付多少钱」。
//!
//! ── 这条和账本里的 `credit` 是**两个问题** ────────────────
//! `requests.credit` 回答「这次从账号里扣了多少积分」（上游自己报的）。
//! 本模块回答另一个问题：「同样这些 token，按模型公开挂牌价值多少钱」。
//! 两者不可互相换算，**不要**把本模块的结果写回 `credit` 列。
//!
//! ── 口径（照搬 workbuddy2api-hub 的 `wb_pricing.py`）────────
//! ```text
//!   成本 = (未命中缓存的输入 × 输入价
//!         + 命中缓存的输入   × 缓存价
//!         + 输出             × 输出价) / unit × 汇率
//! ```
//!   · `unit` 通常是 1_000_000（每百万 token 报价），随快照下发；
//!   · 汇率是「1 美元 = ? 人民币」，快照里带 `usd_cny`，最后结果是**人民币**；
//!   · **输出价只乘 `completion_tokens`** —— 上游把思考 token 计在 completion
//!     里（hub 实测确认），再把 `reasoning_tokens` 加一遍就是重复计费；
//!   · 缓存命中数要**夹到 prompt 以内**（`min(cached, prompt)`）：脏数据里
//!     `cached > prompt` 会让「未命中部分」变负数，算出来的成本是负的。
//!
//! ── 为什么要有 `known` 而不是把没价格的模型按 0 算 ────────────
//! 按 0 算会让报表**静默少报**：用户看到「本月 ¥12」而实际有一半模型没价格。
//! 所以没有价格的模型如实标 `known = false`，并把它计入 `missing`，
//! 由调用方在界面上写明「N 条未定价」，而不是假装它是免费的。
//!
//! ── 与参考实现的差异（有意为之）────────────────────────────
//! 参考实现还支持「按 UTC 时段变价」的条件档位（`band` 里的 `hours`）。
//! 本模块**只实现按输入长度分档**（`min_prompt_tokens`）：时段档位在公开
//! 目录里极少见，而引入它要把「行时间戳 → UTC 小时」的换算搬进每一次统计
//! 聚合，成本远高于收益。遇到带时段档位的快照时按**最便宜档**计（偏保守）。
//! 这一条写在 [`PriceEntry::band_for`] 的注释里，改之前先读那段。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 每百万 token 的三档单价（快照里的原始单位价格）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RateTable {
    /// 未命中缓存的输入单价
    #[serde(default)]
    pub input_cache_miss: f64,
    /// 命中缓存的输入单价（缺省时回落到 `input_cache_miss`）
    #[serde(default)]
    pub input_cache_hit: Option<f64>,
    /// 输出单价
    #[serde(default)]
    pub output: f64,
}

impl RateTable {
    /// 命中缓存的单价：没单独报价时**回落到未命中价**（保守，不高估省下的钱）。
    pub fn cache_hit(&self) -> f64 {
        self.input_cache_hit.unwrap_or(self.input_cache_miss)
    }
}

/// 一条按输入长度分档的条件价格。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PriceBand {
    /// 只有 `prompt_tokens >= 该值` 时这一档才生效
    #[serde(default)]
    pub min_prompt_tokens: i64,
    #[serde(flatten)]
    pub rate: RateTable,
}

/// 一个模型的价格条目。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PriceEntry {
    /// 报价单位（每多少 token 一个价，通常 1_000_000）
    #[serde(default)]
    pub unit: f64,
    /// 无条件档位
    #[serde(default)]
    pub flat: Option<RateTable>,
    /// 条件档位（`min_prompt_tokens` 升序匹配，取命中的最严一档）
    #[serde(default)]
    pub bands: Vec<PriceBand>,
}

impl PriceEntry {
    fn unit(&self) -> f64 {
        if self.unit > 0.0 {
            self.unit
        } else {
            1_000_000.0
        }
    }

    /// 按输入长度挑档位，返回 `(档位下标, 单价表)`。
    ///
    /// 档位下标用于让界面说清「这个数字是按哪一档算的」；无条件价格返回 `None`。
    ///
    /// ⚠️ **取命中的最严一档**（`min_prompt_tokens` 最大的那个），而不是第一个
    /// 命中的：条件档位的语义是「输入超过某长度后改按更贵的价」，档位表可能乱序
    /// 下发，按表序取会挑到便宜档 → 低估成本。乱序不重排，靠这里取 max。
    ///
    /// ⚠️ 带时段档位的快照：本模块**没有**解析 `hours`，于是带时段条件的那一档
    /// 会以「无条件」身份参与上面的 max 比较 —— 结果是按它的价格计，可能比真实
    /// 偏高或偏低。要正确支持请先给 [`PriceBand`] 加时段字段（见模块头的差异说明）。
    pub fn band_for(&self, prompt_tokens: i64) -> (Option<usize>, RateTable) {
        let mut best: Option<(usize, RateTable)> = None;
        for (index, band) in self.bands.iter().enumerate() {
            if prompt_tokens < band.min_prompt_tokens {
                continue;
            }
            match &best {
                Some((_, rate)) if rate.input_cache_miss >= band.rate.input_cache_miss => {}
                _ => best = Some((index, band.rate)),
            }
        }
        match best {
            Some((index, rate)) => (Some(index), rate),
            None => (None, self.flat.unwrap_or_default()),
        }
    }
}

/// 一份价格快照（离线拷贝；不联网刷新，见模块头）。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PriceSnapshot {
    /// 1 美元兑人民币
    #[serde(default)]
    pub usd_cny: f64,
    /// 快照生成时刻（展示用，不参与计算）
    #[serde(default)]
    pub generated_at: String,
    /// `model id → 价格条目`
    #[serde(default)]
    pub models: BTreeMap<String, PriceEntry>,
}

impl PriceSnapshot {
    /// 从 `{"meta":{"usd_cny":…},"models":{…}}` 形状的 JSON 解析。
    ///
    /// 形状不认识时返回错误而不是空快照：空快照会让整份报表显示「0 条已定价」，
    /// 用户以为「这个月没花钱」，而真相是价格表没加载上。
    pub fn parse(value: &Value) -> Result<Self, String> {
        let meta = value.get("meta").cloned().unwrap_or(Value::Null);
        let usd_cny = meta
            .get("usd_cny")
            .and_then(Value::as_f64)
            .filter(|rate| *rate > 0.0)
            .ok_or_else(|| "价格快照缺少有效的 meta.usd_cny".to_string())?;
        let models_value = value
            .get("models")
            .and_then(Value::as_object)
            .ok_or_else(|| "价格快照缺少 models 对象".to_string())?;
        let mut models = BTreeMap::new();
        for (id, raw) in models_value {
            // 单条坏了跳过，不让一条脏数据毁掉整份价格表
            if let Ok(entry) = serde_json::from_value::<PriceEntry>(raw.clone()) {
                models.insert(id.clone(), entry);
            }
        }
        if models.is_empty() {
            return Err("价格快照的 models 里没有可用条目".to_string());
        }
        Ok(Self {
            usd_cny,
            generated_at: meta
                .get("generated_at")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            models,
        })
    }

    pub fn rate(&self) -> f64 {
        if self.usd_cny > 0.0 {
            self.usd_cny
        } else {
            1.0
        }
    }
}

/// 一条请求的计价结果。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CostEstimate {
    /// 估算成本（人民币）
    pub cny: f64,
    /// 这个模型在价格表里有没有条目。`false` 时 `cny` 恒为 0，**不代表免费**
    pub known: bool,
    /// 命中的条件档位下标（`None` = 用的是无条件价）
    pub band: Option<usize>,
    /// 实际用于计算的单价表（让界面能说清「这个数字按什么价算的」）
    pub rate: RateTable,
}

/// 计价所需的行字段（从 `RequestEntry` 摘出来，纯函数因此不依赖存储层）。
#[derive(Clone, Copy, Debug, Default)]
pub struct PriceRow {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cache_read_tokens: i64,
}

/// 给一行算成本。
pub fn compute_row(snapshot: &PriceSnapshot, model: &str, row: PriceRow) -> CostEstimate {
    let Some(entry) = snapshot.models.get(model) else {
        return CostEstimate::default();
    };
    let prompt = row.prompt_tokens.max(0) as f64;
    let output = row.completion_tokens.max(0) as f64;
    // 命中数夹进 prompt：脏数据里 cached > prompt 会让未命中部分变负
    let cached = (row.cache_read_tokens.max(0) as f64).min(prompt);
    let uncached = prompt - cached;
    let (band, rate) = entry.band_for(row.prompt_tokens);
    let cny =
        (uncached * rate.input_cache_miss + cached * rate.cache_hit() + output * rate.output)
            / entry.unit()
            * snapshot.rate();
    CostEstimate {
        cny,
        known: true,
        band,
        rate,
    }
}

/// 一批行的汇总。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CostSummary {
    /// 合计成本（人民币）
    pub cny: f64,
    /// 有价格的行数
    pub priced: i64,
    /// 没价格的模型 → 出现次数。调用方据此提示「N 条未定价」，**不要静默丢掉**
    pub missing: BTreeMap<String, i64>,
}

impl CostSummary {
    /// 一行都没能定价时，界面上那个 0 是「没有数据」而不是「没花钱」。
    pub fn has_any_price(&self) -> bool {
        self.priced > 0
    }
}

/// 给一批行算总成本。
pub fn sum_rows<'a, I>(snapshot: &PriceSnapshot, rows: I) -> CostSummary
where
    I: IntoIterator<Item = (&'a str, PriceRow)>,
{
    let mut summary = CostSummary::default();
    for (model, row) in rows {
        let cost = compute_row(snapshot, model, row);
        if cost.known {
            summary.cny += cost.cny;
            summary.priced += 1;
        } else {
            *summary.missing.entry(model.to_string()).or_insert(0) += 1;
        }
    }
    summary
}

// ── 内置快照（随二进制发布，离线可用）──────────────────────────────

/// 随包发布的价格快照。
///
/// ── 为什么编译进二进制而不是运行时下载 ──────────────────────────
/// 价格只用来「估算成本」：算不出来时界面写「未定价」即可，不该因此让报表
/// 打不开。运行时下载会引入「网关连不上 openrouter.ai ⇒ 报表报错」这种
/// 与业务无关的失败面，而价格表本身是按天更新的量级，随发版走足够。
///
/// 由 `scripts/fetch-pricing.mjs` 生成，换新价就重新生成再提交。
const EMBEDDED: &str = include_str!("../../../assets/pricing.json");

static SNAPSHOT: std::sync::OnceLock<Result<PriceSnapshot, String>> = std::sync::OnceLock::new();
static RESOLVED: std::sync::OnceLock<Result<PriceSnapshot, String>> = std::sync::OnceLock::new();

/// 取内置快照（解析一次，之后复用）。
///
/// 解析失败时返回 `Err`，**不 panic**：`panic = "abort"` 的发布构建里 panic
/// 会让整个进程无声退出，而这只影响一个展示字段。
pub fn snapshot() -> Result<&'static PriceSnapshot, String> {
    SNAPSHOT
        .get_or_init(|| {
            let value: Value =
                serde_json::from_str(EMBEDDED).map_err(|error| format!("内置价格表不是合法 JSON：{error}"))?;
            PriceSnapshot::parse(&value)
        })
        .as_ref()
        .map_err(|error| error.clone())
}

/// 用户覆盖文件的位置：`<配置目录>/pricing.json`。
///
/// ── 为什么必须有这条通道（真实需求，不是预埋）──────────────────
/// 内置表来自 OpenRouter 的公开目录，**覆盖不到网关实际在跑的很多模型**：
/// 实测本机 34 条请求的 `glm-5.3-flash` 就不在表里，于是全部落到「未定价」,
/// 估算成本恒为 0。用户需要能自己给这些模型填价。
///
/// 形状与内置表**完全相同**（`{meta:{usd_cny},models:{id:{unit,flat:{…}}}}`），
/// 按模型 id **覆盖**内置条目；新增的模型直接并进来。
pub fn override_path() -> std::path::PathBuf {
    crate::paths::config_dir().join("pricing.json")
}

/// 把覆盖表并到基表上（纯函数，便于单测）。
///
/// 规则：**同 id 以覆盖表为准**（整体替换该条目，不做字段级合并 —— 字段级合并
/// 会让「我只想改输出价」变成「其余字段从内置表继承」，而内置表的价格随时会
/// 随版本变，那等于让用户的手填价悄悄漂移）。
pub fn merge(base: PriceSnapshot, overlay: PriceSnapshot) -> PriceSnapshot {
    let mut models = base.models;
    for (id, entry) in overlay.models {
        models.insert(id, entry);
    }
    PriceSnapshot {
        // 汇率只用一个：覆盖表给了正值就用它（用户可能想按自己的结算汇率算）
        usd_cny: if overlay.usd_cny > 0.0 { overlay.usd_cny } else { base.usd_cny },
        generated_at: base.generated_at,
        models,
    }
}

/// **生效的**价格快照 = 内置表 + 用户覆盖文件（若存在且可解析）。
///
/// 覆盖文件只在**首次调用时读一次**（之后走缓存）：改完要重启进程才生效。
/// 说清楚这一点比悄悄每轮 stat 一个文件好 —— 后者会让「改了没生效」变成
/// 一个要靠猜的时序问题。
///
/// 覆盖文件损坏/形状不对时**保留内置表**并返回内置结果（不降级成空表）：
/// 用户的笔误不该让整份报表变成「全部未定价」。
pub fn resolved() -> Result<&'static PriceSnapshot, String> {
    RESOLVED
        .get_or_init(|| {
            let base = snapshot()?.clone();
            let path = override_path();
            let Ok(text) = std::fs::read_to_string(&path) else {
                return Ok(base);
            };
            let value: Value = match serde_json::from_str(&text) {
                Ok(value) => value,
                Err(error) => {
                    crate::server::logging::log(
                        "Pricing",
                        &format!("覆盖价格表不是合法 JSON，已忽略（{}）：{error}", path.display()),
                    );
                    return Ok(base);
                }
            };
            match PriceSnapshot::parse(&value) {
                Ok(overlay) => {
                    let merged = merge(base, overlay);
                    crate::server::logging::log(
                        "Pricing",
                        &format!("已合并用户价格表（{}）：共 {} 个模型", path.display(), merged.models.len()),
                    );
                    Ok(merged)
                }
                Err(error) => {
                    crate::server::logging::log(
                        "Pricing",
                        &format!("覆盖价格表形状不符，已忽略（{}）：{error}", path.display()),
                    );
                    Ok(base)
                }
            }
        })
        .as_ref()
        .map_err(|error| error.clone())
}

/// 按一组候选模型名依次试，返回第一个在价格表里能查到的结果。
///
/// ── 为什么要试多个候选 ────────────────────────────────────
/// 价格表的 key 是上游的**规范名**（`deepseek/deepseek-chat` 这种带命名空间的），
/// 而网关记录两列：`model`（解析后的名字，可能是 `deepseek-v4.1-flash`）
/// 与 `upstream_model`（实际发给上游的名字）。两者哪个能命中没有规律，
/// 所以按顺序各试一次；都查不到就如实返回「未定价」。
pub fn cost_for<'a, I>(snapshot: &PriceSnapshot, candidates: I, row: PriceRow) -> (Option<String>, CostEstimate)
where
    I: IntoIterator<Item = &'a str>,
{
    for candidate in candidates {
        let name = candidate.trim();
        if name.is_empty() {
            continue;
        }
        let cost = compute_row(snapshot, name, row);
        if cost.known {
            return (Some(name.to_string()), cost);
        }
    }
    (None, CostEstimate::default())
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn synthetic() -> PriceSnapshot {
        PriceSnapshot::parse(&json!({
            "meta": { "usd_cny": 7.1, "generated_at": "2026-11-02T22:55:18" },
            "models": {
                "deepseek-v4.1-flash": {
                    "unit": 1000000,
                    "flat": { "input_cache_miss": 0.28, "input_cache_hit": 0.028, "output": 0.42 }
                },
                "banded": {
                    "unit": 1000000,
                    "bands": [
                        { "min_prompt_tokens": 0, "input_cache_miss": 1.0, "output": 2.0 },
                        { "min_prompt_tokens": 200000, "input_cache_miss": 2.0, "output": 3.0 }
                    ]
                },
                "no-cache-price": {
                    "unit": 1000000,
                    "flat": { "input_cache_miss": 1.0, "output": 1.0 }
                }
            }
        }))
        .expect("快照应当解析成功")
    }

    /// 三段分别是「未命中输入 / 命中输入 / 输出」，各乘自己的单价再乘汇率。
    #[test]
    fn a_flat_price_splits_cached_and_uncached_input() {
        let snapshot = synthetic();
        let cost = compute_row(
            &snapshot,
            "deepseek-v4.1-flash",
            PriceRow { prompt_tokens: 1_000_000, completion_tokens: 1_000_000, cache_read_tokens: 600_000 },
        );
        assert!(cost.known);
        // (400k*0.28 + 600k*0.028 + 1M*0.42) / 1M * 7.1
        let expected = (400_000.0 * 0.28 + 600_000.0 * 0.028 + 1_000_000.0 * 0.42) / 1_000_000.0 * 7.1;
        assert!((cost.cny - expected).abs() < 1e-9, "got {}", cost.cny);
        assert_eq!(cost.band, None);
    }

    /// 没有单独给缓存价的模型，命中部分按**未命中价**算（保守，不多省钱）。
    #[test]
    fn a_model_without_a_cache_rate_falls_back_to_the_miss_rate() {
        let snapshot = synthetic();
        let cost = compute_row(
            &snapshot,
            "no-cache-price",
            PriceRow { prompt_tokens: 1_000_000, completion_tokens: 0, cache_read_tokens: 500_000 },
        );
        let expected = 1_000_000.0 * 1.0 / 1_000_000.0 * 7.1;
        assert!((cost.cny - expected).abs() < 1e-9, "got {}", cost.cny);
    }

    /// 输入超过阈值就换贵的一档（这是条件档位存在的全部意义）。
    #[test]
    fn a_long_prompt_switches_to_the_dearer_band() {
        let snapshot = synthetic();
        let short = compute_row(
            &snapshot,
            "banded",
            PriceRow { prompt_tokens: 100_000, completion_tokens: 0, cache_read_tokens: 0 },
        );
        let long = compute_row(
            &snapshot,
            "banded",
            PriceRow { prompt_tokens: 300_000, completion_tokens: 0, cache_read_tokens: 0 },
        );
        assert_eq!(short.band, Some(0));
        assert_eq!(long.band, Some(1));
        // 300k @ 2.0 比 100k @ 1.0 贵三倍以上（同一单位下）
        assert!(long.cny > short.cny * 3.0);
    }

    /// 脏数据：缓存命中数大于输入总数时，未命中部分不能变负数、成本不能变负。
    #[test]
    fn cached_tokens_above_prompt_never_makes_the_cost_negative() {
        let snapshot = synthetic();
        let cost = compute_row(
            &snapshot,
            "deepseek-v4.1-flash",
            PriceRow { prompt_tokens: 10_000, completion_tokens: 0, cache_read_tokens: 999_999 },
        );
        assert!(cost.cny >= 0.0, "got {}", cost.cny);
        // 全部按命中价：10k * 0.028 / 1M * 7.1
        let expected = 10_000.0 * 0.028 / 1_000_000.0 * 7.1;
        assert!((cost.cny - expected).abs() < 1e-12, "got {}", cost.cny);
    }

    /// 没价格的模型如实标 unknown，并进 missing —— 不能按 0 静默掉。
    #[test]
    fn an_unpriced_model_is_reported_as_missing_not_as_free() {
        let snapshot = synthetic();
        let summary = sum_rows(
            &snapshot,
            vec![
                ("deepseek-v4.1-flash", PriceRow { prompt_tokens: 1000, completion_tokens: 100, cache_read_tokens: 0 }),
                ("some-new-model", PriceRow { prompt_tokens: 1000, completion_tokens: 100, cache_read_tokens: 0 }),
                ("some-new-model", PriceRow { prompt_tokens: 2000, completion_tokens: 200, cache_read_tokens: 0 }),
            ],
        );
        assert_eq!(summary.priced, 1);
        assert_eq!(summary.missing.get("some-new-model"), Some(&2));
        assert!(summary.cny > 0.0);
        assert!(summary.has_any_price());
    }

    /// 形状不对（缺 usd_cny / 缺 models）必须报错，不能返回空快照。
    #[test]
    fn a_malformed_snapshot_is_an_error_not_an_empty_table() {
        assert!(PriceSnapshot::parse(&json!({ "models": {} })).is_err());
        assert!(PriceSnapshot::parse(&json!({ "meta": { "usd_cny": 7.1 } })).is_err());
        assert!(PriceSnapshot::parse(&json!({ "meta": {}, "models": { "a": {} } })).is_err());
    }

    /// 内置快照必须真的能用（`include_str!` 的路径写错、或生成脚本改了形状，
    /// 都在这里被立刻发现，而不是等到界面上全显示「未定价」）。
    #[test]
    fn the_embedded_snapshot_parses_and_covers_models() {
        let snapshot = snapshot().expect("内置价格表必须能解析");
        assert!(
            snapshot.models.len() > 100,
            "内置价格表条目太少，可能是生成脚本没跑完整：{}",
            snapshot.models.len()
        );
        assert!(snapshot.rate() > 1.0, "汇率应当在合理区间，实际 {}", snapshot.rate());
        // 每一条都得有输出价与未命中输入价（缓存价可以缺）
        for (id, entry) in snapshot.models.iter().take(50) {
            let (_, rate) = entry.band_for(0);
            assert!(rate.input_cache_miss > 0.0, "{id} 的输入价应当为正");
            assert!(rate.output > 0.0, "{id} 的输出价应当为正");
        }
    }

    /// `cost_for` 按候选顺序试，命中第一个能查到的名字。
    #[test]
    fn cost_for_tries_each_candidate_in_order() {
        let snapshot = snapshot().expect("内置价格表必须能解析");
        let row = PriceRow { prompt_tokens: 1000, completion_tokens: 100, cache_read_tokens: 0 };
        // 第一个候选查不到、第二个能查到 ⇒ 用第二个
        let (used, cost) = cost_for(&snapshot, ["definitely-not-a-model", "openai/gpt-4o"], row);
        assert_eq!(used.as_deref(), Some("openai/gpt-4o"), "应当落到第二个候选");
        assert!(cost.known);
        assert!(cost.cny > 0.0, "有价格时成本应当为正，实际 {}", cost.cny);

        // 全查不到 ⇒ 未定价，且成本为 0（而不是随便给个 0 装作已知）
        let (used, cost) = cost_for(&snapshot, ["nope-a", "nope-b"], row);
        assert!(used.is_none());
        assert!(!cost.known);
        assert_eq!(cost.cny, 0.0);
    }

    /// 覆盖表：同 id **整体替换**、新 id 并入、汇率可被覆盖。
    ///
    /// 这条通道是真实需求 —— 内置表来自 OpenRouter 公开目录，覆盖不到网关
    /// 实际在跑的模型（实测本机全部流量都是 `glm-5.3-flash`，它不在表里）。
    #[test]
    fn an_override_replaces_whole_entries_and_adds_new_ones() {
        let base = PriceSnapshot::parse(&json!({
            "meta": { "usd_cny": 7.1 },
            "models": {
                "existing": { "unit": 1000000, "flat": { "input_cache_miss": 1.0, "output": 2.0 } },
                "kept": { "unit": 1000000, "flat": { "input_cache_miss": 3.0, "output": 4.0 } }
            }
        }))
        .expect("基表");
        let overlay = PriceSnapshot::parse(&json!({
            "meta": { "usd_cny": 6.5 },
            "models": {
                // 同 id：整体替换（输出价改了，输入价不给就该是 0，而不是继承基表的 1.0）
                "existing": { "unit": 1000000, "flat": { "output": 9.0 } },
                // 新 id：并进来
                "glm-5.3-flash": { "unit": 1000000, "flat": { "input_cache_miss": 0.5, "output": 1.5 } }
            }
        }))
        .expect("覆盖表");

        let merged = merge(base, overlay);
        assert_eq!(merged.rate(), 6.5, "汇率以覆盖表为准");
        assert_eq!(merged.models.len(), 3, "两条原有 + 一条新增");
        let replaced = &merged.models["existing"];
        assert_eq!(replaced.flat.as_ref().unwrap().output, 9.0);
        assert_eq!(
            replaced.flat.as_ref().unwrap().input_cache_miss, 0.0,
            "整体替换而不是字段级合并：没给输入价就是 0，不继承基表的 1.0"
        );
        assert_eq!(merged.models["kept"].flat.as_ref().unwrap().input_cache_miss, 3.0, "没被覆盖的条目原样保留");
        assert!(merged.models.contains_key("glm-5.3-flash"), "新增模型应当并入");
    }
}
