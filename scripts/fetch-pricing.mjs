#!/usr/bin/env node
/**
 * 生成模型价格快照 `server/assets/pricing.json`。
 *
 * ── 数据来源 ────────────────────────────────────────────────
 * OpenRouter 的公开模型目录（`https://openrouter.ai/api/v1/models`）。它给的是
 * **每 token 的美元**字符串，这里统一折算成**每百万 token 的美元**，并在
 * `meta.usd_cny` 里带上汇率 —— 运行时按「价 / unit × 汇率」算人民币。
 *
 * ── 这是「等值成本估算」──────────────────────────────────────
 * 它回答「同样这些 token 按公开挂牌价值多少钱」，**不是**上游实际扣了多少。
 * 网关自己的账本是 `requests.credit` 列，两者不可换算，也别互相覆盖。
 *
 * ── 只保留三个字段 ──────────────────────────────────────────
 * 上游条目有大段描述与架构信息（整包 800KB+），本网关只用来算钱，所以只留
 * `input_cache_miss` / `input_cache_hit` / `output` 与 `unit`。
 *
 * ── `input_cache_hit` 缺省「不写」而不是写 0 ─────────────────
 * 没缓存价的模型占多数。写 0 会让「缓存命中」被当成免费，系统性低估成本；
 * 运行时 `RateTable::cache_hit()` 的语义是「没单独报价时回落到未命中价」。
 *
 * 用法：
 *   node scripts/fetch-pricing.mjs            # 写入默认路径
 *   node scripts/fetch-pricing.mjs --dry-run  # 只打印统计，不落盘
 */
import { writeFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const SOURCE = 'https://openrouter.ai/api/v1/models'
const UNIT = 1_000_000
const USD_CNY = 7.1

const here = dirname(fileURLToPath(import.meta.url))
const OUT = join(here, '..', 'desktop-tauri', 'src-tauri', 'server', 'assets', 'pricing.json')

/** 每 token 的美元 → 每百万 token 的美元；读不出返回 undefined。 */
function perMillion(raw) {
  if (raw === undefined || raw === null) return undefined
  const value = Number.parseFloat(String(raw).trim())
  if (!Number.isFinite(value) || value <= 0) return undefined
  return Math.round(value * UNIT * 1e6) / 1e6
}

const dryRun = process.argv.includes('--dry-run')

const response = await fetch(SOURCE)
if (!response.ok) {
  console.error(`[pricing] 拉取失败：HTTP ${response.status}`)
  process.exit(1)
}
const payload = await response.json()
const models = {}
for (const item of payload.data ?? []) {
  const id = typeof item?.id === 'string' ? item.id.trim() : ''
  if (!id) continue
  const pricing = item.pricing ?? {}
  const miss = perMillion(pricing.prompt)
  // 没有输入价就没法算钱：跳过而不是记成 0（记 0 会被当成免费）
  if (miss === undefined) continue
  const rate = { input_cache_miss: miss, output: perMillion(pricing.completion) ?? 0 }
  const hit = perMillion(pricing.input_cache_read) ?? perMillion(pricing.cache_read)
  if (hit !== undefined) rate.input_cache_hit = hit
  models[id] = { unit: UNIT, flat: rate }
}

const snapshot = {
  meta: {
    generated_at: new Date().toISOString().slice(0, 19),
    base_currency: 'CNY',
    usd_cny: USD_CNY,
    source: SOURCE,
    unit: UNIT,
    note:
      '由 scripts/fetch-pricing.mjs 生成；单价为 OpenRouter 挂牌价（每百万 token 美元），' +
      '运行时按 meta.usd_cny 折算人民币。这是「等值成本估算」，不是上游实际扣费。',
  },
  models,
}

const ids = Object.keys(models)
console.log(`[pricing] 模型 ${ids.length} 条，缓存价覆盖 ${ids.filter(id => models[id].flat.input_cache_hit !== undefined).length} 条`)
if (dryRun) {
  console.log('[pricing] --dry-run：不写文件')
} else {
  await writeFile(OUT, JSON.stringify(snapshot, Object.keys(snapshot).sort(), 1) + '\n', 'utf8')
  console.log(`[pricing] 已写入 ${OUT}`)
}
